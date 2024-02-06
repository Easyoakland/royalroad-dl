use leaky_bucket::RateLimiter;
use regex::Regex;
use royalroad_dl::BufferedIter;
use scraper::Html;
use std::{
    borrow::Cow,
    num::NonZeroU64,
    path::PathBuf,
    sync::{Arc, OnceLock},
    time::Duration,
};
use tokio::{
    fs::File,
    io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt},
};
use url::Url;

mod selectors;
const END_HTML: &str = "</body></html>";

/// Convert path to something that can be saved to file.
pub fn sanitize_path(path: &str) -> Cow<'_, str> {
    static REGEX: OnceLock<Regex> = OnceLock::new();
    let regex = REGEX.get_or_init(|| Regex::new(r#"[^\w\d]+"#).unwrap());
    regex.replace_all(path, "_")
}

/// Incremental periodic downloader. Useful on slow connections, going offline, or because online content has a tendency to disappear.
/// Like receiving new content in the mail.
#[derive(Debug, Clone, bpaf::Bpaf)]
#[bpaf(options, version)]
struct Options {
    /// Custom output path.
    #[bpaf(short, long)]
    path: Option<PathBuf>,
    /// Minimum ms per request. Can't be zero.
    #[bpaf(short, long, fallback(NonZeroU64::new(1500).unwrap()), display_fallback)]
    time_limit: NonZeroU64,
    /// Concurrent connections limit. Zero indicates no limit.
    #[bpaf(short, long, fallback(4), display_fallback)]
    connections: usize,
    /// Incremental download. Auto-detect previously downloaded and only download new.
    #[bpaf(short, long)]
    incremental: bool,
    /// The main page (e.g. table of contents) of the content to download.
    #[bpaf(positional("URL"))]
    url: Url,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // parse cli opts
    let opt = options().run();

    let main_html = Html::parse_document(&reqwest::get(opt.url.clone()).await?.text().await?);

    let main_title = main_html
        .select(selectors::title_selector())
        .map(|x| x.inner_html())
        .next()
        .unwrap_or_default();
    // Create output file.
    let path = opt.path.unwrap_or(PathBuf::from(format!(
        "{}.html",
        sanitize_path(&main_title)
    )));
    println!("Saving to {}", path.display());
    // Write main title.
    let incremental = path.exists() && opt.incremental;
    let mut f = if incremental {
        File::options()
            .read(true)
            .write(true)
            .open(path.clone())
            .await?
    } else {
        File::create(path.clone()).await?
    };
    if incremental {
        // Seek to just before end of file to avoid overwriting previous downloads.
        // Truncate previous `END_HTML` if it exists so it isn't duplicated when added back at the end.
        f.seek(std::io::SeekFrom::End(
            -i64::try_from(END_HTML.len() * 2).unwrap(),
        ))
        .await?;

        let mut dst = String::new();
        f.read_to_string(&mut dst).await?;
        if let Some(previous_file_end) = dst
            .rfind("</html>")
            .and_then(|idx| dst[..idx].rfind("</body>"))
        {
            f.set_len(
                f.metadata().await?.len() - u64::try_from(dst.len()).unwrap()
                    + (u64::try_from(previous_file_end)).unwrap(),
            )
            .await?;
            f.seek(std::io::SeekFrom::End(0)).await?;
        }
    } else {
        // Write title and file start.
        f.write_all(
            format!(
                r#"<html><head><meta charset="UTF-8"><title>{}</title></head><body>"#,
                main_title
            )
            .as_bytes(),
        )
        .await?;
    }

    // Get chapters with a rate limit.
    let limiter = Arc::new(
        RateLimiter::builder()
            .fair(false)
            .initial(1)
            .interval(Duration::from_millis(opt.time_limit.get()))
            .build(),
    );
    let chapters_len;
    let chapter_responses = {
        let mut chapters = main_html
            .select(selectors::chapter_link_selector()) // table of chapters
            .map(|x| x.attr("data-url").expect("data-url attribute in selector")) // url for table entry
            .map(|x| opt.url.join(x).unwrap()); // absolute url from relative url
        let chapters = if incremental {
            // Filter past chapters already downloaded
            let mut f = File::open(path).await?;
            let mut dst = String::new();
            f.read_to_string(&mut dst).await?;
            let previously_downloaded = Html::parse_document(&dst);

            // Skip last downloaded chapter if it exists.
            if let Some(last_downloaded_chapter) = previously_downloaded
                .select(selectors::downloaded_chapter_selector())
                .last()
                .and_then(|x| x.attr("href"))
                .map(|x| Url::parse(x).expect("valid url"))
            {
                let _ = chapters
                    .by_ref()
                    .skip_while(|x| *x != last_downloaded_chapter) // skip up to most recently downloaded
                    .next(); // and including most recently downloaded
            };

            chapters.collect()
        } else {
            chapters.collect::<Vec<_>>()
        };
        chapters_len = chapters.len();
        // Buffer tasks while handling for concurrency.
        let chapter_responses = BufferedIter::new(
            chapters.into_iter().enumerate().map(move |(i, url)| {
                let limiter = limiter.clone();
                tokio::spawn(async move {
                    limiter.acquire_one().await;
                    println!("Downloading {}/{}: {}", i + 1, chapters_len, url);
                    let res = (url.clone(), reqwest::get(url).await);
                    res
                })
            }),
            opt.connections,
        );
        chapter_responses
    };
    // Save each chapter to file.
    for (i, handle) in chapter_responses.enumerate() {
        let (url, chapter_response) = handle.await?;
        let mut chapter_html = Html::parse_document(&chapter_response?.text().await?);

        // Write chapter title.
        let chapter_title = chapter_html
            .select(selectors::title_selector())
            .map(|x| x.inner_html())
            .next()
            .expect("chapter title"); // TODO: don't panic
        let chapter_title = chapter_title
            .strip_suffix(&main_title)
            .and_then(|x| x.strip_suffix(" - "))
            .unwrap_or(&chapter_title);
        f.write_all(
            format!(
                r#"<h1><a class="chapter" href="{}">{}</a></h1>"#,
                url, chapter_title
            )
            .as_bytes(),
        )
        .await?;

        // Remove bad paragraphs.
        let bad_paragraphs = chapter_html
            .select(selectors::paragraph_selector())
            .filter_map(|x| {
                if selectors::is_warning(&x.inner_html()) {
                    println!("Removing {}/{}: {} ", i + 1, chapters_len, x.inner_html());
                    Some(x.id())
                } else {
                    None
                }
            })
            .collect::<Vec<_>>();
        for id in bad_paragraphs {
            chapter_html
                .tree
                .get_mut(id)
                .expect("id selected from tree")
                .detach();
        }

        // Write chapter content.
        let chapter_content = chapter_html
            .select(selectors::chapter_content_selector())
            .map(|x| x.html())
            .next()
            .expect("chapter body"); // TODO don't panic

        f.write_all(chapter_content.as_bytes()).await?;
    }

    f.write_all(END_HTML.as_bytes()).await?;
    f.shutdown().await?;
    Ok(())
}
