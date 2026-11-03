//! The documentation site (docs/16 §4, 1.0): the repository's Markdown as
//! static pages, with the web application beside them under `app/`.
//!
//! `cargo xtask site [--out DIR]` writes `DIR/site` (default
//! `dist/release/site`). Links between documents become links between pages;
//! links to other repository files point to the repository. The output
//! depends only on the sources, so rebuilding gives the same files.
use super::{Options, Result, copy, write};
use pulldown_cmark::{CowStr, Event, HeadingLevel, Options as Markdown, Parser, Tag, TagEnd, html};
use std::collections::BTreeMap;
use std::path::{Component, Path, PathBuf};

const REPOSITORY: &str = "https://github.com/KvasFan/Planyrith/blob/main/";

/// Source (relative to the repository) → page (relative to the site), in
/// navigation order.
fn pages(root: &Path) -> Result<Vec<(PathBuf, PathBuf)>> {
    let mut pages = vec![
        ("README.md".into(), "index.html".into()),
        ("packaging/INSTALL.md".into(), "install.html".into()),
        (
            "docs/qa/model-a-room.md".into(),
            "guide/model-a-room.html".into(),
        ),
        ("CHANGELOG.md".into(), "changelog.html".into()),
        ("docs/README.md".into(), "spec/index.html".into()),
    ];
    for folder in ["docs", "docs/releases"] {
        let mut names: Vec<String> = std::fs::read_dir(root.join(folder))?
            .filter_map(|e| e.ok()?.file_name().into_string().ok())
            .filter(|n| n.ends_with(".md") && n != "README.md")
            .collect();
        names.sort();
        let to = if folder == "docs" {
            "spec"
        } else {
            "spec/releases"
        };
        for name in names {
            let page = format!("{to}/{}", name.replace(".md", ".html"));
            pages.push((format!("{folder}/{name}").into(), page.into()));
        }
    }
    Ok(pages)
}

/// Images the pages show, copied beside them.
const ASSETS: [(&str, &str); 3] = [
    ("docs/images", "spec/images"),
    ("docs/qa", "guide"),
    ("assets/brand", "brand"),
];

/// `a/b/../c` → `a/c`, without touching the file system.
fn normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for part in path.components() {
        match part {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            other => out.push(other),
        }
    }
    out
}

/// The relative URL from page `from` to site path `to`.
fn relative(from: &Path, to: &Path) -> String {
    let depth = from.parent().map_or(0, |p| p.components().count());
    let mut url = "../".repeat(depth);
    url.push_str(&to.to_string_lossy().replace('\\', "/"));
    url
}

/// GitHub-style heading anchor.
fn slug(text: &str) -> String {
    text.chars()
        .filter_map(|c| {
            if c.is_alphanumeric() || c == '-' || c == '_' {
                Some(c.to_lowercase().collect::<String>())
            } else if c == ' ' {
                Some("-".into())
            } else {
                None
            }
        })
        .collect()
}

struct Site {
    /// Repository path → site path, for every page and copied asset.
    map: BTreeMap<PathBuf, PathBuf>,
}

impl Site {
    /// Rewrites a link found in `source` for the page `page`.
    fn link(&self, source: &Path, page: &Path, url: &str) -> String {
        if url.contains("://") || url.starts_with('#') || url.starts_with("mailto:") {
            return url.to_owned();
        }
        let (path, anchor) = url.split_once('#').map_or((url, ""), |(p, a)| (p, a));
        let target = normalize(&source.parent().unwrap_or(Path::new("")).join(path));
        let anchor = if anchor.is_empty() {
            String::new()
        } else {
            format!("#{anchor}")
        };
        match self.map.get(&target) {
            Some(to) => format!("{}{anchor}", relative(page, to)),
            None => format!(
                "{REPOSITORY}{}{anchor}",
                target.to_string_lossy().replace('\\', "/")
            ),
        }
    }

    /// Rewrites the `src`, `href` and `srcset` attributes of raw HTML.
    fn html(&self, source: &Path, page: &Path, text: &str) -> String {
        let mut out = String::new();
        let mut rest = text;
        while let Some(at) = ["src=\"", "href=\"", "srcset=\""]
            .iter()
            .filter_map(|a| rest.find(a).map(|i| (i, a.len())))
            .min()
        {
            let (start, len) = at;
            out.push_str(&rest[..start + len]);
            rest = &rest[start + len..];
            let end = rest.find('"').unwrap_or(rest.len());
            out.push_str(&self.link(source, page, &rest[..end]));
            rest = &rest[end..];
        }
        out.push_str(rest);
        out
    }

    fn render(&self, source: &Path, page: &Path, markdown: &str) -> (String, String) {
        let options = Markdown::ENABLE_TABLES
            | Markdown::ENABLE_STRIKETHROUGH
            | Markdown::ENABLE_TASKLISTS
            | Markdown::ENABLE_FOOTNOTES;
        let mut title = String::new();
        let mut events = Vec::new();
        let mut heading: Option<(HeadingLevel, Vec<Event>)> = None;
        for event in Parser::new_ext(markdown, options) {
            let event = match event {
                Event::Start(Tag::Link {
                    link_type,
                    dest_url,
                    title,
                    id,
                }) => Event::Start(Tag::Link {
                    link_type,
                    dest_url: CowStr::from(self.link(source, page, &dest_url)),
                    title,
                    id,
                }),
                Event::Start(Tag::Image {
                    link_type,
                    dest_url,
                    title,
                    id,
                }) => Event::Start(Tag::Image {
                    link_type,
                    dest_url: CowStr::from(self.link(source, page, &dest_url)),
                    title,
                    id,
                }),
                Event::Html(text) => Event::Html(CowStr::from(self.html(source, page, &text))),
                Event::InlineHtml(text) => {
                    Event::InlineHtml(CowStr::from(self.html(source, page, &text)))
                }
                other => other,
            };
            match event {
                Event::Start(Tag::Heading { level, .. }) => heading = Some((level, Vec::new())),
                Event::End(TagEnd::Heading(_)) => {
                    if let Some((level, inner)) = heading.take() {
                        let text: String = inner
                            .iter()
                            .filter_map(|e| match e {
                                Event::Text(t) | Event::Code(t) => Some(t.as_ref()),
                                _ => None,
                            })
                            .collect();
                        if title.is_empty() {
                            title = text.clone();
                        }
                        let mut body = String::new();
                        html::push_html(&mut body, inner.into_iter());
                        let n = level as usize;
                        events.push(Event::Html(CowStr::from(format!(
                            "<h{n} id=\"{}\">{body}</h{n}>\n",
                            slug(&text)
                        ))));
                    }
                }
                other => match &mut heading {
                    Some((_, inner)) => inner.push(other),
                    None => events.push(other),
                },
            }
        }
        let mut body = String::new();
        html::push_html(&mut body, events.into_iter());
        (title, body)
    }
}

fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

fn layout(page: &Path, title: &str, body: &str) -> String {
    let nav = [
        ("index.html", "Planyrith"),
        ("install.html", "Install"),
        ("guide/model-a-room.html", "Guide"),
        ("spec/index.html", "Specification"),
        ("changelog.html", "Changes"),
        ("app/index.html", "Open in the browser"),
    ]
    .iter()
    .map(|(to, label)| {
        let current = if Path::new(to) == page {
            " aria-current=\"page\""
        } else {
            ""
        };
        format!(
            "<a href=\"{}\"{current}>{label}</a>",
            relative(page, Path::new(to))
        )
    })
    .collect::<Vec<_>>()
    .join("\n");
    format!(
        r#"<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>{title} — Planyrith</title>
<link rel="stylesheet" href="{css}">
<link rel="icon" href="{icon}" type="image/svg+xml">
</head>
<body>
<header><nav>
{nav}
</nav></header>
<main>
{body}</main>
</body>
</html>
"#,
        title = escape(title),
        css = relative(page, Path::new("site.css")),
        icon = relative(page, Path::new("icon.svg")),
    )
}

const CSS: &str = r#"/* Planyrith documentation site (docs/16 §4): the application's dark palette. */
:root {
  color-scheme: dark light;
  --view: #1c1c1f; --window: #242428; --header: #2e2e32; --card: #2c2c31;
  --border: #38383c; --text: #ededf0; --weak: #9c9ca4; --accent: #78aeed;
}
@media (prefers-color-scheme: light) {
  :root {
    --view: #ffffff; --window: #fafafb; --header: #ebebed; --card: #f2f2f4;
    --border: #dadade; --text: #2b2b30; --weak: #6a6a72; --accent: #1c63c4;
  }
}
* { box-sizing: border-box; }
body {
  margin: 0; background: var(--window); color: var(--text);
  font: 16px/1.6 system-ui, -apple-system, "Segoe UI", Cantarell, Ubuntu, sans-serif;
}
header { background: var(--header); border-bottom: 1px solid var(--border); position: sticky; top: 0; }
nav { max-width: 60rem; margin: 0 auto; padding: .5rem 1rem; display: flex; flex-wrap: wrap; gap: .25rem; }
nav a { color: var(--text); text-decoration: none; padding: .35rem .75rem; border-radius: 6px; }
nav a:hover { background: var(--card); }
nav a[aria-current] { background: var(--card); color: var(--accent); }
nav a:first-child { font-weight: 700; }
nav a:last-child { margin-left: auto; background: #3584e4; color: #fff; }
main { max-width: 60rem; margin: 0 auto; padding: 1.5rem 1rem 4rem; }
a { color: var(--accent); }
h1, h2, h3 { line-height: 1.25; }
h1 { font-size: 2rem; }
h2 { margin-top: 2.5rem; padding-bottom: .3rem; border-bottom: 1px solid var(--border); }
code { background: var(--card); padding: .1em .35em; border-radius: 4px; font-size: .9em; }
pre { background: var(--view); border: 1px solid var(--border); border-radius: 8px; padding: 1rem; overflow-x: auto; }
pre code { background: none; padding: 0; }
table { border-collapse: collapse; display: block; overflow-x: auto; margin: 1rem 0; }
th, td { border: 1px solid var(--border); padding: .4rem .7rem; text-align: left; vertical-align: top; }
th { background: var(--header); }
tr:nth-child(even) td { background: var(--card); }
img { max-width: 100%; height: auto; border-radius: 8px; }
blockquote { margin: 1rem 0; padding: .1rem 1rem; border-left: 4px solid var(--accent); background: var(--card); border-radius: 0 8px 8px 0; }
"#;

/// `cargo xtask site`: renders the documentation site.
pub(super) fn build(options: &Options) -> Result<()> {
    let out = options.out.join("site");
    if out.exists() {
        std::fs::remove_dir_all(&out)?;
    }
    let pages = pages(&options.root)?;
    let mut map: BTreeMap<PathBuf, PathBuf> = pages.iter().cloned().collect();
    let mut assets = Vec::new();
    for (from, to) in ASSETS {
        let mut names: Vec<String> = std::fs::read_dir(options.root.join(from))?
            .filter_map(|e| e.ok()?.file_name().into_string().ok())
            .filter(|n| n.ends_with(".png") || n.ends_with(".svg") || n.ends_with(".jpg"))
            .collect();
        names.sort();
        for name in names {
            let (source, target) = (Path::new(from).join(&name), Path::new(to).join(&name));
            map.insert(source.clone(), target.clone());
            assets.push((source, target));
        }
    }
    let site = Site { map };
    for (source, page) in &pages {
        let markdown = std::fs::read_to_string(options.root.join(source))?;
        let (title, body) = site.render(source, page, &markdown);
        write(&out.join(page), layout(page, &title, &body))?;
    }
    for (source, target) in assets {
        copy(&options.root.join(source), &out.join(target))?;
    }
    write(&out.join("site.css"), CSS)?;
    copy(
        &options.root.join("crates/planyrith-gui/web/icon.svg"),
        &out.join("icon.svg"),
    )?;
    println!("{}", out.display());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The real site: every relative link and image resolves to a file (the
    /// web application is added beside it at publication).
    #[test]
    fn the_site_has_no_broken_links() -> Result<()> {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .ok_or("root")?;
        let out = std::env::temp_dir().join(format!("planyrith-site-{}", std::process::id()));
        let options = Options {
            root: root.to_path_buf(),
            bins: out.clone(),
            out: out.clone(),
            appimagetool: None,
            runtime: None,
        };
        build(&options)?;
        let site = out.join("site");
        let mut broken = Vec::new();
        for (_, page) in pages(root)? {
            let html = std::fs::read_to_string(site.join(&page))?;
            for attribute in ["href=\"", "src=\"", "srcset=\""] {
                for piece in html.split(attribute).skip(1) {
                    let url = piece.split('"').next().unwrap_or("");
                    let path = url.split('#').next().unwrap_or("");
                    if url.contains("://") || path.is_empty() || url.starts_with("mailto:") {
                        continue;
                    }
                    let target = normalize(&page.parent().unwrap_or(Path::new("")).join(path));
                    if !target.starts_with("app") && !site.join(&target).is_file() {
                        broken.push(format!("{}: {url}", page.display()));
                    }
                }
            }
        }
        std::fs::remove_dir_all(&out)?;
        assert!(broken.is_empty(), "{broken:#?}");
        Ok(())
    }

    #[test]
    fn links_follow_the_pages() {
        let site = Site {
            map: [
                (
                    "docs/09-file-formats.md".into(),
                    "spec/09-file-formats.html".into(),
                ),
                ("CHANGELOG.md".into(), "changelog.html".into()),
            ]
            .into_iter()
            .collect(),
        };
        let (source, page) = (Path::new("docs/README.md"), Path::new("spec/index.html"));
        assert_eq!(
            site.link(source, page, "09-file-formats.md#4-versioning"),
            "../spec/09-file-formats.html#4-versioning"
        );
        assert_eq!(
            site.link(source, page, "../CHANGELOG.md"),
            "../changelog.html"
        );
        assert_eq!(
            site.link(source, page, "../AGENTS.md"),
            format!("{REPOSITORY}AGENTS.md")
        );
        assert_eq!(
            site.link(source, page, "https://x.org/a.md"),
            "https://x.org/a.md"
        );
        assert_eq!(
            slug("4. Versioning and migration"),
            "4-versioning-and-migration"
        );
        let (title, body) = site.render(source, page, "# Title `x`\n\n[f](09-file-formats.md)\n");
        assert_eq!(title, "Title x");
        assert!(body.contains(r#"<h1 id="title-x">Title <code>x</code></h1>"#));
        assert!(body.contains(r#"href="../spec/09-file-formats.html""#));
    }
}
