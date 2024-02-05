use leaky_bucket::RateLimiter;
use regex::Regex;
use royalroad_dl::BufferedIter;
use scraper::{selector, Html, Selector};
use std::{
    borrow::Cow,
    num::NonZeroU64,
    path::PathBuf,
    sync::{Arc, OnceLock},
    time::Duration,
};
use tokio::{fs::File, io::AsyncWriteExt};
use url::Url;

fn title_selector() -> &'static Selector {
    static CELL: OnceLock<Selector> = OnceLock::new();
    CELL.get_or_init(|| selector::Selector::parse("title").unwrap())
}
fn chapter_link_selector() -> &'static Selector {
    static CELL: OnceLock<Selector> = OnceLock::new();
    CELL.get_or_init(|| {
        selector::Selector::parse(r#"#chapters tr[data-url^="/fiction/"]"#).unwrap()
    })
}
fn chapter_content_selector() -> &'static Selector {
    static CELL: OnceLock<Selector> = OnceLock::new();
    CELL.get_or_init(|| selector::Selector::parse("div.chapter-content").unwrap())
}
fn paragraph_selector() -> &'static Selector {
    static CELL: OnceLock<Selector> = OnceLock::new();
    CELL.get_or_init(|| selector::Selector::parse("p").unwrap())
}
/// Convert path to something that can be saved to file.
fn sanitize_path(path: &str) -> Cow<'_, str> {
    static REGEX: OnceLock<Regex> = OnceLock::new();
    let regex = REGEX.get_or_init(|| Regex::new(r#"[^\w\d]"#).unwrap());
    regex.replace_all(path, "_")
}
/// If paragraph contains a warning
fn is_warning(msg: &str) -> bool {
    static REGEX: OnceLock<Regex> = OnceLock::new();
    msg.len() < 150
        && {
            let regex = REGEX.get_or_init(|| Regex::new(r#"stolen|(on Amazon)|purloined|pilfered|misappropriated|report|content|unauthorized|(Royal Road)|theft|story|novel|appropriated|narrative|permission|illicitly"#).unwrap());
            regex.find_iter(msg).count() >= 3
        }
}
/// Warning s

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
    /// Max concurrent connections. Zero indicates no limit.
    #[bpaf(short, long, fallback(1), display_fallback)]
    connections: usize,
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
        .select(title_selector())
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
    let mut f = File::create(path).await?;
    f.write_all(
        format!(
            r#"<html><head><meta charset="UTF-8"><title>{}</title></head><body>"#,
            main_title
        )
        .as_bytes(),
    )
    .await?;

    // Get chapters with a rate limit.
    let limiter = Arc::new(
        RateLimiter::builder()
            .fair(false)
            .initial(4)
            .interval(Duration::from_millis(opt.time_limit.get()))
            .build(),
    );
    let chapter_responses = |debug| {
        let chapters = main_html
            .select(chapter_link_selector()) // table of chapters
            .map(|x| x.attr("data-url").expect("data-url attribute in selector")) // url for table entry
            .map(|x| opt.url.join(x).unwrap()); // absolute url from relative url
        let chapters = chapters.collect::<Vec<_>>();
        let chapters_len = chapters.len();
        let limiter = limiter.clone();
        // Buffer tasks while handling for concurrency.
        let chapter_responses = BufferedIter::new(
            chapters.into_iter().enumerate().map(move |(i, url)| {
                let limiter = limiter.clone();
                tokio::spawn(async move {
                    limiter.acquire_one().await;
                    if debug {
                        println!("Downloading {}/{}: {}", i + 1, chapters_len, url)
                    };
                    let res = (url.clone(), reqwest::get(url).await);
                    res
                })
            }),
            opt.connections,
        );
        chapter_responses
    };
    // Download each chapter twice. Compare content. Remove text that doesn't match.
    for (handle, handle2) in std::iter::zip(chapter_responses(true), chapter_responses(false)) {
        let (url, chapter_response) = handle.await?;
        let mut chapter_html = Html::parse_document(&chapter_response?.text().await?);

        // Write chapter title.
        let chapter_title = chapter_html
            .select(title_selector())
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

        // Compare to second download and remove inconsistent paragraphs.
        let chapter_html2 = Html::parse_document(&handle2.await?.1?.text().await?);
        let inconsistent_paragraphs = std::iter::zip(
            chapter_html.select(paragraph_selector()),
            chapter_html2.select(paragraph_selector()),
        )
        .filter_map(|(x, y)| {
            if x.inner_html() != y.inner_html() || (is_warning(&x.inner_html())) {
                println!("Removing: {} ", x.inner_html());
                println!("{}", y.inner_html());

                Some(x.id())
            } else {
                None
            }
        })
        .collect::<Vec<_>>();
        for id in inconsistent_paragraphs {
            chapter_html
                .tree
                .get_mut(id)
                .expect("unchanged tree since selected id")
                .detach()
        }

        // Write chapter content.
        let chapter_content = chapter_html
            .select(chapter_content_selector())
            .map(|x| x.html())
            .next()
            .expect("chapter body"); // TODO don't panic

        f.write_all(chapter_content.as_bytes()).await?;
    }

    f.write_all(b"</body></html>").await?;
    f.shutdown().await?;
    Ok(())
}
