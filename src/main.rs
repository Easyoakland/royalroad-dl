use leaky_bucket::RateLimiter;
use regex::Regex;
use royalroad_dl::BufferedIter;
use scraper::Html;
use std::{
    borrow::Cow,
    io::BufRead,
    num::NonZeroU64,
    path::PathBuf,
    sync::{Arc, OnceLock},
    time::Duration,
};
use tokio::{
    fs::File,
    io::{AsyncSeekExt, AsyncWriteExt},
};
use url::Url;

mod selectors;
const END_HTML: &str = "</body></html>";

/// Convert path to something that can be saved to file.
pub fn sanitize_path(path: &str) -> Cow<'_, str> {
    static REGEX: OnceLock<Regex> = OnceLock::new();
    // See https://en.wikipedia.org/wiki/Filename#Comparison_of_filename_limitations
    let regex = REGEX.get_or_init(|| Regex::new(r#"[\x00-\x1F\x7F"*/:<>?\\|]+"#).unwrap());
    regex.replace_all(path, "_")
}

/// - Seek to after the last content previously downloaded in preparation for writing new content.
/// - Retrieves last known chapter url if found
/// # Note
/// Does blocking io
// TODO: don't do blocking io. Will need to impl AsyncRevBufReader
async fn start_incremental_append(f: &mut tokio::fs::File) -> std::io::Result<Option<Url>> {
    use rev_buf_reader::RevBufReader;

    static REGEX: OnceLock<Regex> = OnceLock::new();

    let mut rev_lines = RevBufReader::new(f.try_clone().await?.into_std().await).lines();
    // Get the offset end of the previous download for seeking later.
    let mut reverse_offset = 0;
    let mut found_end = false;
    for line in rev_lines.by_ref() {
        let line = line?;
        if let Some(idx) = line.rfind("</body>") {
            reverse_offset += line.len() - idx;
            found_end = true;
            break;
        } else {
            reverse_offset += line.len();
        }
    }

    // Get the most recently downloaded chapter
    let chapter_url = rev_lines
        .find_map(|line| {
            line.map(|line| {
                REGEX
                    .get_or_init(|| Regex::new(r#"href="(.*?)""#).unwrap())
                    .captures(&line)
                    .and_then(|x| x.get(1).and_then(|x| Url::parse(x.as_str()).ok()))
            })
            .transpose()
        })
        .transpose()?;

    if found_end {
        // Start appending at end of file before last `END_HTML`.
        f.seek(std::io::SeekFrom::End(
            -i64::try_from(reverse_offset).unwrap(),
        ))
        .await?;
    }
    Ok(chapter_url)
}

/// Incremental periodic downloader. Useful on slow connections, going offline, or because online content has a tendency to disappear.
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
    // Parse cli options.
    let opt = options().run();

    let client = reqwest::Client::builder().build().unwrap();

    // Get main document.
    let main_html = Html::parse_document(&client.get(opt.url.clone()).send().await?.text().await?);

    // Extract title.
    let main_title = main_html
        .select(selectors::title_selector())
        .map(|x| x.inner_html())
        .next()
        .unwrap_or_default();

    // Start output file. Either create new or reuse previous if incremental download.
    let path = opt.path.unwrap_or(PathBuf::from(format!(
        "{}.html",
        sanitize_path(
            main_title
                .strip_suffix(" | Royal Road")
                .unwrap_or(&main_title)
        )
    )));
    println!("Saving to {}", path.display());
    let incremental = path.exists() && opt.incremental;
    let mut f = if incremental {
        let backup_path = {
            let mut out = path.clone().into_os_string();
            out.push(".bk");
            out
        };
        println!("Backup to {}", std::path::Path::new(&backup_path).display());
        tokio::fs::copy(&path, &backup_path).await?;
        File::options().read(true).write(true).open(&path).await?
    } else {
        File::create(&path).await?
    };
    let last_downloaded_chapter = if incremental {
        start_incremental_append(&mut f).await?
    } else {
        None
    };
    // If no known chapter to resume from
    if last_downloaded_chapter.is_none() {
        // Start writing file from beginning.
        f.seek(std::io::SeekFrom::Start(0)).await?;
        // Write title and file headers.
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
    let (chapters_len, chapter_responses) = {
        let mut chapters = main_html
            .select(selectors::chapter_link_selector()) // table of chapters
            .map(|x| x.attr("data-url").expect("data-url attribute in selector")) // url for table entry
            .map(|x| opt.url.join(x).unwrap()) // absolute url from relative url
            .enumerate()
            .collect::<Vec<_>>();

        let chapters_len = chapters.len();

        if incremental {
            // Skip last downloaded chapter if it exists in the main page.
            // If it doesn't exist, don't skip anything as that likely indicates previously
            // downloaded chapters are no longer available and all available chapters were posted
            // after those already downloaded previously.
            if let Some(chapter) = last_downloaded_chapter {
                if let Some(pos) = chapters.iter().position(|(_, x)| *x == chapter) {
                    chapters.drain(..=pos);
                };
            };
        }

        // GET urls and Buffer tasks for concurrency.
        let chapter_responses = BufferedIter::new(
            chapters.into_iter().map(move |(i, url)| {
                let limiter = limiter.clone();
                let client = client.clone();
                tokio::spawn(async move {
                    limiter.acquire_one().await;
                    println!("Downloading {}/{}: {}", i + 1, chapters_len, url);
                    (i, url.clone(), client.get(url).send().await)
                })
            }),
            opt.connections,
        );
        (chapters_len, chapter_responses)
    };
    // Save each chapter to file.
    for handle in chapter_responses {
        let (i, url, chapter_response) = handle.await?;
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

        // Write chapter content and end with `END_HTML` in case of ctrl-c.
        let mut chapter_content = chapter_html
            .select(selectors::chapter_content_selector())
            .map(|x| x.html())
            .next()
            .expect("chapter body"); // TODO don't panic
        chapter_content.push_str(END_HTML);
        f.write_all(chapter_content.as_bytes()).await?;

        // Seek before `END_HTML` so it is overwritten on next chapter content
        f.seek(std::io::SeekFrom::Current(
            -i64::try_from(END_HTML.len()).unwrap(),
        ))
        .await?;
    }

    f.shutdown().await?;
    Ok(())
}
