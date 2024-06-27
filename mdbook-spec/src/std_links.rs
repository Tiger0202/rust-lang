use mdbook::book::Chapter;
use once_cell::sync::Lazy;
use regex::{Captures, Regex};
use std::collections::HashSet;
use std::fmt::Write as _;
use std::fs;
use std::io::{self, Write as _};
use std::process::{self, Command};
use tempfile::TempDir;

/// A markdown link (without the brackets) that might possibly be a link to
/// the standard library using rustdoc's intra-doc notation.
const STD_LINK: &str = r"(?: [a-z]+@ )?
                         (?: std|core|alloc|proc_macro|test )
                         (?: ::[A-Za-z0-9_!:<>{}()\[\]]+ )?";

/// The Regex for a markdown link that might be a link to the standard library.
static STD_LINK_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(&format!(
        r"(?x)
            (?:
                ( \[`[^`]+`\] ) \( ({STD_LINK}) \)
            )
            | (?:
                ( \[`{STD_LINK}`\] )
            )
         "
    ))
    .unwrap()
});

/// The Regex used to extract the std links from the HTML generated by rustdoc.
static STD_LINK_EXTRACT_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r#"<li><a [^>]*href="(https://doc.rust-lang.org/[^"]+)""#).unwrap());

/// The Regex for a markdown link definition.
static LINK_DEF_RE: Lazy<Regex> = Lazy::new(|| {
    // This is a pretty lousy regex for a link definition. It doesn't
    // handle things like blockquotes, code blocks, etc. Using a
    // markdown parser isn't really feasible here, it would be nice to
    // improve this.
    Regex::new(r#"(?m)^(?<label>\[[^]]+\]): *(?<dest>.*)"#).unwrap()
});

/// Converts links to the standard library to the online documentation in a
/// fashion similar to rustdoc intra-doc links.
pub fn std_links(chapter: &Chapter) -> String {
    let links = collect_markdown_links(chapter);
    if links.is_empty() {
        return chapter.content.clone();
    }

    // Write a Rust source file to use with rustdoc to generate intra-doc links.
    let tmp = TempDir::with_prefix("mdbook-spec-").unwrap();
    run_rustdoc(&tmp, &links, &chapter);

    // Extract the links from the generated html.
    let generated =
        fs::read_to_string(tmp.path().join("doc/a/index.html")).expect("index.html generated");
    let urls: Vec<_> = STD_LINK_EXTRACT_RE
        .captures_iter(&generated)
        .map(|cap| cap.get(1).unwrap().as_str())
        .collect();
    if urls.len() != links.len() {
        eprintln!(
            "error: expected rustdoc to generate {} links, but found {} in chapter {} ({:?})",
            links.len(),
            urls.len(),
            chapter.name,
            chapter.source_path.as_ref().unwrap()
        );
        process::exit(1);
    }

    // Replace any disambiguated links with just the disambiguation.
    let mut output = STD_LINK_RE
        .replace_all(&chapter.content, |caps: &Captures| {
            if let Some(dest) = caps.get(2) {
                // Replace destination parenthesis with a link definition (square brackets).
                format!("{}[{}]", &caps[1], dest.as_str())
            } else {
                caps[0].to_string()
            }
        })
        .to_string();

    // Append the link definitions to the bottom of the chapter.
    write!(output, "\n").unwrap();
    for ((link, dest), url) in links.iter().zip(urls) {
        // Convert links to be relative so that links work offline and
        // with the linkchecker.
        let url = relative_url(url, chapter);
        if let Some(dest) = dest {
            write!(output, "[{dest}]: {url}\n").unwrap();
        } else {
            write!(output, "{link}: {url}\n").unwrap();
        }
    }

    output
}

/// Collects all markdown links, excluding those that already have link definitions.
///
/// Returns a `Vec` of `(link, Option<dest>)` where markdown text like
/// ``[`std::fmt`]`` would return that as a link. The dest is optional, for
/// example ``[`Option`](std::option::Option)`` would have the part in
/// parentheses as the dest.
fn collect_markdown_links(chapter: &Chapter) -> Vec<(&str, Option<&str>)> {
    let mut links: Vec<_> = STD_LINK_RE
        .captures_iter(&chapter.content)
        .map(|cap| {
            if let Some(no_dest) = cap.get(3) {
                (no_dest.as_str(), None)
            } else {
                (
                    cap.get(1).unwrap().as_str(),
                    Some(cap.get(2).unwrap().as_str()),
                )
            }
        })
        .collect();
    if links.is_empty() {
        return vec![];
    }
    links.sort();
    links.dedup();
    // Remove any links that already have a link definition. We don't want
    // to override what the author explicitly specified.
    let existing_labels: HashSet<_> = LINK_DEF_RE
        .captures_iter(&chapter.content)
        .map(|cap| cap.get(1).unwrap().as_str())
        .collect();
    links.retain(|(link, dest)| {
        let mut tmp = None;
        let label: &str = dest.map_or(link, |d| {
            tmp = Some(format!("[`{d}`]"));
            tmp.as_deref().unwrap()
        });
        !existing_labels.contains(label)
    });

    links
}

/// Generates links using rustdoc.
///
/// This takes the given links and creates a temporary Rust source file
/// containing those links within doc-comments, and then runs rustdoc to
/// generate intra-doc links on them.
///
/// The output will be in the given `tmp` directory.
fn run_rustdoc(tmp: &TempDir, links: &[(&str, Option<&str>)], chapter: &Chapter) {
    let src_path = tmp.path().join("a.rs");
    // Allow redundant since there could some in-scope things that are
    // technically not necessary, but we don't care about (like
    // [`Option`](std::option::Option)).
    let mut src = format!(
        "#![deny(rustdoc::broken_intra_doc_links)]\n\
         #![allow(rustdoc::redundant_explicit_links)]\n"
    );
    for (link, dest) in links {
        write!(src, "//! - {link}").unwrap();
        if let Some(dest) = dest {
            write!(src, "({})", dest).unwrap();
        }
        src.push('\n');
    }
    writeln!(
        src,
        "extern crate alloc;\n\
         extern crate proc_macro;\n\
         extern crate test;\n"
    )
    .unwrap();
    fs::write(&src_path, &src).unwrap();
    let output = Command::new("rustdoc")
        .arg("--edition=2021")
        .arg(&src_path)
        .current_dir(tmp.path())
        .output()
        .expect("rustdoc installed");
    if !output.status.success() {
        eprintln!(
            "error: failed to extract std links ({:?}) in chapter {} ({:?})\n",
            output.status,
            chapter.name,
            chapter.source_path.as_ref().unwrap()
        );
        io::stderr().write_all(&output.stderr).unwrap();
        process::exit(1);
    }
}

static DOC_URL: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"^https://doc.rust-lang.org/(?:nightly|beta|stable|dev|1\.[0-9]+\.[0-9]+)").unwrap()
});

/// Converts a URL to doc.rust-lang.org to be relative.
fn relative_url(url: &str, chapter: &Chapter) -> String {
    // Set SPEC_RELATIVE=0 to disable this, which can be useful for working locally.
    if std::env::var("SPEC_RELATIVE").as_deref() != Ok("0") {
        let Some(url_start) = DOC_URL.shortest_match(url) else {
            eprintln!("expected rustdoc URL to start with {DOC_URL:?}, got {url}");
            std::process::exit(1);
        };
        let url_path = &url[url_start..];
        let num_dots = chapter.path.as_ref().unwrap().components().count();
        let dots = vec![".."; num_dots].join("/");
        format!("{dots}{url_path}")
    } else {
        url.to_string()
    }
}
