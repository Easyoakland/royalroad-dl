//! Selectors for content

use regex::{Regex, RegexBuilder};
use scraper::{selector, Selector};
use std::{collections::HashSet, sync::OnceLock};

pub fn title_selector() -> &'static Selector {
    static CELL: OnceLock<Selector> = OnceLock::new();
    CELL.get_or_init(|| selector::Selector::parse("title").unwrap())
}
/// Select chapters from urls table of contents.
pub fn chapter_link_selector() -> &'static Selector {
    static CELL: OnceLock<Selector> = OnceLock::new();
    CELL.get_or_init(|| {
        selector::Selector::parse(r#"#chapters tr[data-url^="/fiction/"]"#).unwrap()
    })
}
pub fn chapter_content_selector() -> &'static Selector {
    static CELL: OnceLock<Selector> = OnceLock::new();
    CELL.get_or_init(|| selector::Selector::parse("div.chapter-content").unwrap())
}
pub fn paragraph_selector() -> &'static Selector {
    static CELL: OnceLock<Selector> = OnceLock::new();
    CELL.get_or_init(|| selector::Selector::parse("p").unwrap())
}
/// If paragraph contains a warning
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
