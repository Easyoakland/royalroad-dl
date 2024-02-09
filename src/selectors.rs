//! Selectors for content

use scraper::{selector, Selector};
use std::sync::OnceLock;

pub fn title() -> &'static Selector {
    static CELL: OnceLock<Selector> = OnceLock::new();
    CELL.get_or_init(|| selector::Selector::parse("title").unwrap())
}
/// Select chapters from urls table of contents.
pub fn chapter_links() -> &'static Selector {
    static CELL: OnceLock<Selector> = OnceLock::new();
    CELL.get_or_init(|| {
        selector::Selector::parse(r#"#chapters tr[data-url^="/fiction/"]"#).unwrap()
    })
}
pub fn chapter_contents() -> &'static Selector {
    static CELL: OnceLock<Selector> = OnceLock::new();
    CELL.get_or_init(|| selector::Selector::parse("div.chapter-content").unwrap())
}
/* pub fn paragraphs() -> &'static Selector {
    static CELL: OnceLock<Selector> = OnceLock::new();
    CELL.get_or_init(|| selector::Selector::parse("p").unwrap())
} */
pub fn warning_paragraphs() -> &'static Selector {
    static CELL: OnceLock<Selector> = OnceLock::new();
    // Warning paragraphs are always included in html. They are hidden by inline css matching the following.
    CELL.get_or_init(|| selector::Selector::parse(r#"[class^=cj],[class^=cm]"#).unwrap())
}
/*
/// If paragraph content contains a warning
/// Not needed because of [`warning_paragraphs()`] selector
pub fn is_warning(msg: &str) -> bool {
    static REGEX: OnceLock<Regex> = OnceLock::new();
    msg.len() < 150 && {
        let regex = REGEX.get_or_init(|| {
            RegexBuilder::new(
                "(on Amazon)\
                |(Royal Road)\
                |appropriated
                |content\
                |illicitly\
                |misappropriated\
                |narrative
                |novel\
                |permission\
                |pilfered\
                |purloined\
                |report\
                |story\
                |taken\
                |theft\
                |unauthorized\
                |stolen",
            )
            .case_insensitive(true)
            .build()
            .unwrap()
        });
        regex
            .find_iter(msg)
            .map(|x| x.as_str())
            .collect::<HashSet<_>>()
            .len()
            >= 3
    }
}
 */
pub fn downloaded_chapters() -> &'static Selector {
    static CELL: OnceLock<Selector> = OnceLock::new();
    CELL.get_or_init(|| selector::Selector::parse(r#"h1 > a[class="chapter"][href]"#).unwrap())
}
