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

/// Layout of page changed.
#[derive(thiserror::Error, Debug, Clone, PartialEq, Eq)]
enum PageLayoutError {
    #[error("main page title not found")]
    MainTitle,
    #[error("no chapter links found")]
    ChapterLinks,
    #[error("chapter title not found")]
    ChapterTitle,
    #[error("chapter body not found")]
    ChapterBody,
}
/// The error type for custom errors with the downloader.
#[derive(thiserror::Error, Debug)]
enum Error {
    #[error("Page layout different from expected. Perhaps the website changed?: {0}")]
    Layout(#[from] PageLayoutError),
    #[error("{0}")]
    Request(#[from] reqwest::Error),
}

/// Wrapper over [`Url`] that compares urls as equal if they represent the same fiction regardless of url content (e.g. with same uuid but different title as same).
#[derive(Clone, Debug)]
struct ChapterUrl(pub Url);
impl PartialEq for ChapterUrl {
    fn eq(&self, other: &Self) -> bool {
        let Some(iter) = self
            .0
            .path_segments()
            .zip(other.0.path_segments())
            .map(|(p1, p2)| core::iter::zip(p1, p2))
        else {
            return false;
        };
        iter.enumerate().all(|(i, (p1, p2))| i == 2 || p1 == p2)
    }
}
impl Eq for ChapterUrl {}
impl From<Url> for ChapterUrl {
    fn from(value: Url) -> Self {
        Self(value)
    }
}
impl From<ChapterUrl> for Url {
    fn from(value: ChapterUrl) -> Self {
        value.0
    }
}

/// Convert path to something that can be saved to file.
pub fn sanitize_path(path: &str) -> Cow<'_, str> {
    static REGEX: OnceLock<Regex> = OnceLock::new();
    // See https://en.wikipedia.org/wiki/Filename#Comparison_of_filename_limitations
    let regex = REGEX.get_or_init(|| Regex::new(r#"[\x00-\x1F\x7F"*/:<>?\\|]+"#).unwrap());
    regex.replace_all(path, "_")
}

/// - Seek to after the last content previously downloaded in preparation for writing new content.
/// - Retrieves cached chapters.
async fn start_incremental_append(f: &mut tokio::fs::File) -> std::io::Result<Vec<ChapterUrl>> {
    let previous_download = {
        let mut s = String::new();
        f.read_to_string(&mut s).await?;
        s
    };

    // Get cached chapters.
    let previous_html = Html::parse_document(&previous_download);
    let cached_chapters = previous_html
        .select(selectors::downloaded_chapters())
        .filter_map(|x| {
            x.attr("href")
                .and_then(|x| Url::parse(x).ok())
                .map(Into::into)
        })
        .collect::<Vec<_>>();

    // Start appending at end of file before last `END_HTML`.
    if let Some(offset) = previous_download.rfind("</body>") {
        f.seek(std::io::SeekFrom::Start(offset.try_into().unwrap()))
            .await?;
    }
    Ok(cached_chapters)
}

/// Get final content for chapter from `chapter_response`.
///
/// May use `chapter_progress_msg` when logging.
async fn chapter_response_to_content(
    chapter_progress_msg: &str,
    chapter_response: reqwest::Response,
    main_title: &str,
) -> Result<String, Error> {
    let url = chapter_response.url().to_owned();
    let mut chapter_html = Html::parse_document(&chapter_response.text().await?);

    let mut out = String::new();

    // Write chapter title.
    let chapter_title = chapter_html
        .select(selectors::title())
        .map(|x| x.inner_html())
        .next()
        .ok_or(PageLayoutError::ChapterTitle)?;
    let chapter_title = chapter_title
        .strip_suffix(&main_title)
        .and_then(|x| x.strip_suffix(" - "))
        .unwrap_or(&chapter_title);
    out.push_str(&format!(
        r#"<h1><a class="chapter" href="{}">{}</a></h1>"#,
        url, chapter_title
    ));

    // Remove bad paragraphs.
    let bad_paragraphs = chapter_html
        .select(selectors::warning_paragraphs())
        .map(|x| {
            println!("Removing {}: {} ", chapter_progress_msg, x.inner_html());
            x.id()
        })
        .collect::<Vec<_>>();
    for id in bad_paragraphs {
        chapter_html.tree.get_mut(id).unwrap().detach();
    }

    let chapter_content = chapter_html
        .select(selectors::chapter_content())
        .map(|x| x.html())
        .next()
        .ok_or(PageLayoutError::ChapterBody)?;
    out.push_str(&chapter_content);

    Ok(out)
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

#[tokio::main(flavor = "current_thread")]
async fn main() -> anyhow::Result<()> {
    // Parse cli options.
    let opt = options().run();

    let client = reqwest::Client::builder().build().unwrap();

    // Get main document.
    let main_html = Html::parse_document(&client.get(opt.url.clone()).send().await?.text().await?);

    // Extract title.
    let main_title = main_html
        .select(selectors::title())
        .map(|x| x.inner_html())
        .next()
        .ok_or(PageLayoutError::MainTitle)?;

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
    let incremental = opt.incremental && path.exists();
    if !opt.incremental && path.exists() {
        anyhow::bail!("Path already exists. Move the item at the path or pass `--incremental` to use it as previous chapter cache.");
    }
    let mut f = if incremental {
        File::options().read(true).write(true).open(&path).await?
    } else {
        File::create(&path).await?
    };

    // Get previously downloaded chapters as applicable.
    let cached_chapters = if incremental {
        let cached_chapters = start_incremental_append(&mut f).await?;
        if cached_chapters.is_empty() {
            // Will be replacing file so backup first.
            let backup_path = {
                let mut out = path.clone().into_os_string();
                out.push(".bk");
                out
            };
            println!(
                "Couldn't find a previous chapter URL.\nOverwriting file after backing up to {}",
                std::path::Path::new(&backup_path).display()
            );
            tokio::fs::copy(&path, &backup_path).await?;
        }
        cached_chapters
    } else {
        Vec::new()
    };

    // If no known chapter to resume from
    if cached_chapters.is_empty() {
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
            .initial(1)
            .interval(Duration::from_millis(opt.time_limit.get()))
            .build(),
    );
    let (chapters_len, chapter_responses) = {
        let mut chapters = main_html
            .select(selectors::chapter_links()) // table of chapters
            .map(|x| x.attr("data-url").expect("data-url attribute in selector")) // url for table entry
            .map(|x| opt.url.join(x).unwrap().into()) // absolute url from relative url
            .enumerate()
            .collect::<Vec<_>>();
        if chapters.is_empty() {
            return Err(Error::Layout(PageLayoutError::ChapterLinks).into());
        }

        let chapters_len = chapters.len();

        if incremental {
            // Don't download chapters already downloaded.
            chapters.retain(|(_, x)| !cached_chapters.contains(x));
        }

        // GET urls and Buffer tasks for concurrency.
        let chapter_responses = BufferedIter::new(
            chapters.into_iter().map(move |(i, url)| {
                let limiter = limiter.clone();
                let client = client.clone();
                tokio::spawn(async move {
                    limiter.acquire_one().await;
                    println!("Downloading {}/{}: {}", i + 1, chapters_len, url.0);
                    (i, client.get(url.0).send().await)
                })
            }),
            opt.connections,
        );
        (chapters_len, chapter_responses)
    };
    // Save each chapter to file.
    for handle in chapter_responses {
        let (i, chapter_response) = handle.await?;

        // Write chapter content and end with `END_HTML` in case of ctrl-c.
        let mut chapter_content = chapter_response_to_content(
            &format!("{}/{}", i + 1, chapters_len),
            chapter_response?,
            &main_title,
        )
        .await?;
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

#[cfg(test)]
mod tests {
    use crate::ChapterUrl;
    use url::Url;

    #[test]
    fn chapter_url_partial_eq() -> anyhow::Result<()> {
        let chapter_1 = ChapterUrl(Url::parse(
            "https://www.royalroad.com/fiction/12345/the-title/chapter/1234567/chapter_title",
        )?);
        let chapter_2 = ChapterUrl(Url::parse("https://www.royalroad.com/fiction/12345/the-title-but-different/chapter/1234567/chapter_title")?);
        assert_eq!(chapter_1, chapter_2);
        assert_ne!(chapter_1.0, chapter_2.0);
        Ok(())
    }
}
