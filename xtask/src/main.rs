use std::{
    cmp::Ordering,
    env, fs, io,
    path::{Path, PathBuf},
    process::Command,
};

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

const DOC_PAGES: &[&str] = &[
    "SUMMARY.md",
    "index.md",
    "ui-guide.md",
    "architecture.md",
    "message_lifecycle.md",
    "topic_conventions.md",
    "manual_control.md",
    "safety_model.md",
    "roadmap.md",
];

#[derive(Debug)]
struct Tools {
    mdbook_version: String,
}

#[derive(Debug)]
struct Options {
    docs_version: Option<String>,
    docs_out_dir: Option<PathBuf>,
}

#[derive(Debug)]
struct DocVersion {
    dir: String,
    label: String,
    current: bool,
}

fn main() -> Result<()> {
    let root = find_repo_root(&env::current_dir()?).or_else(|_| {
        let manifest_dir = env!("CARGO_MANIFEST_DIR");
        find_repo_root(Path::new(manifest_dir))
    })?;
    let (command, options) = parse_args()?;

    match command.as_str() {
        "docs" => docs(&root, &options),
        _ => fail(format!("unknown command '{command}'. expected: docs")),
    }
}

fn docs(root: &Path, options: &Options) -> Result<()> {
    let tools = read_tools(root)?;
    let version = options
        .docs_version
        .clone()
        .or_else(|| env::var("DOCS_VERSION").ok())
        .or_else(|| env::var("GITHUB_REF_NAME").ok())
        .unwrap_or_else(|| "main".to_string());
    let out_dir = options
        .docs_out_dir
        .clone()
        .unwrap_or_else(|| root.join("target/xtask/docs"));

    generate_docs_site(root, &tools, &version, &out_dir)
}

fn parse_args() -> Result<(String, Options)> {
    let mut args = env::args().skip(1);
    let command = args.next().unwrap_or_else(|| "docs".to_string());
    let mut docs_version = None;
    let mut docs_out_dir = None;

    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--version" => {
                docs_version = Some(
                    args.next()
                        .ok_or_else(|| io::Error::other("--version requires a value"))?,
                );
            }
            "--out-dir" => {
                docs_out_dir =
                    Some(PathBuf::from(args.next().ok_or_else(|| {
                        io::Error::other("--out-dir requires a value")
                    })?));
            }
            _ => return fail(format!("unknown option '{arg}'")),
        }
    }

    Ok((
        command,
        Options {
            docs_version,
            docs_out_dir,
        },
    ))
}

fn find_repo_root(start: &Path) -> Result<PathBuf> {
    for path in start.ancestors() {
        if path.join("Cargo.toml").is_file()
            && path.join("package.json").is_file()
            && path.join("apps/web").is_dir()
        {
            return Ok(path.to_path_buf());
        }
    }
    fail("could not find electrode-web repository root")
}

fn read_tools(root: &Path) -> Result<Tools> {
    let text = fs::read_to_string(root.join("tools.lock"))?;
    for line in text.lines() {
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        if key == "MDBOOK_VERSION" {
            return Ok(Tools {
                mdbook_version: value.trim().to_string(),
            });
        }
    }
    fail("tools.lock is missing MDBOOK_VERSION")
}

fn generate_docs_site(root: &Path, tools: &Tools, version: &str, out_dir: &Path) -> Result<()> {
    let version = normalize_docs_version(version);
    let version_dir_name = docs_dir_name(&version);
    let book_dir = root.join("target/xtask/docs-mdbook");
    println!(
        "generating electrode-web developer docs for {version} into {}",
        out_dir.display()
    );

    ensure_mdbook(&tools.mdbook_version)?;
    fs::create_dir_all(out_dir)?;
    let versions = docs_versions(out_dir, &version_dir_name)?;
    reset_dir(&book_dir)?;
    write_mdbook_source(root, &book_dir, &version, &version_dir_name, &versions)?;
    run(Command::new("mdbook").arg("build").arg(&book_dir))?;

    let version_out_dir = out_dir.join(&version_dir_name);
    reset_dir(&version_out_dir)?;
    copy_dir_recursive(&book_dir.join("book"), &version_out_dir)?;
    write_docs_root_index(out_dir, &version_dir_name)?;
    refresh_docs_version_selectors(out_dir, &version_dir_name)?;
    write_docs_version_redirect_aliases(out_dir, &version_dir_name)?;
    Ok(())
}

fn write_mdbook_source(
    root: &Path,
    book_dir: &Path,
    version: &str,
    version_dir_name: &str,
    versions: &[DocVersion],
) -> Result<()> {
    write_file(&book_dir.join("book.toml"), &render_book_toml(version))?;
    for page in DOC_PAGES {
        copy_file(
            &root.join("docs").join(page),
            &book_dir.join("src").join(page),
        )?;
    }
    copy_file(
        &root.join("docs/theme/electrode.css"),
        &book_dir.join("theme/electrode.css"),
    )?;
    let assets_dir = root.join("docs/assets");
    if assets_dir.is_dir() {
        copy_dir_recursive(&assets_dir, &book_dir.join("src/assets"))?;
    }
    write_file(
        &book_dir.join("theme/version-selector.js"),
        &render_version_selector_js(versions, version_dir_name),
    )
}

fn render_book_toml(version: &str) -> String {
    format!(
        r#"[book]
title = "electrode-web {version}"
description = "Versioned developer documentation for electrode-web."
src = "src"

[output.html]
default-theme = "rust"
preferred-dark-theme = "navy"
git-repository-url = "https://github.com/CogniPilot/electrode-web"
additional-css = ["theme/electrode.css"]
additional-js = ["theme/version-selector.js"]
"#,
        version = escape_toml_basic(version)
    )
}

fn ensure_mdbook(expected_version: &str) -> Result<()> {
    let output = Command::new("mdbook").arg("--version").output();
    let Ok(output) = output else {
        return fail(format!(
            "mdbook {expected_version} is required to generate docs. Install it with: cargo install mdbook --version {expected_version} --locked"
        ));
    };
    if !output.status.success() {
        return fail(format!(
            "mdbook {expected_version} is required to generate docs. Install it with: cargo install mdbook --version {expected_version} --locked"
        ));
    }
    let actual = String::from_utf8(output.stdout)?.trim().to_string();
    let expected = format!("mdbook v{expected_version}");
    if actual != expected {
        return fail(format!(
            "unexpected mdbook version '{actual}', expected '{expected}'"
        ));
    }
    Ok(())
}

fn docs_versions(out_dir: &Path, current_version_dir: &str) -> Result<Vec<DocVersion>> {
    let mut dirs = Vec::new();
    if out_dir.is_dir() {
        for entry in fs::read_dir(out_dir)? {
            let entry = entry?;
            if !entry.file_type()?.is_dir() {
                continue;
            }
            let name = entry.file_name().to_string_lossy().to_string();
            if !name.starts_with('.') {
                dirs.push(name);
            }
        }
    }
    if !dirs.iter().any(|dir| dir == current_version_dir) {
        dirs.push(current_version_dir.to_string());
    }
    dirs.sort_by(|left, right| compare_doc_version_dirs(left, right));
    dirs.dedup();

    Ok(dirs
        .into_iter()
        .map(|dir| DocVersion {
            current: dir == current_version_dir,
            label: doc_version_label(&dir),
            dir,
        })
        .collect())
}

fn compare_doc_version_dirs(left: &str, right: &str) -> Ordering {
    match (doc_version_key(left), doc_version_key(right)) {
        (Some(left), Some(right)) => right.cmp(&left),
        (Some(_), None) => Ordering::Greater,
        (None, Some(_)) => Ordering::Less,
        (None, None) => left.cmp(right),
    }
}

fn doc_version_key(value: &str) -> Option<Vec<u64>> {
    let value = value.strip_prefix('v').unwrap_or(value);
    let mut parts = Vec::new();
    for part in value.split('.') {
        parts.push(part.parse().ok()?);
    }
    (!parts.is_empty()).then_some(parts)
}

fn doc_version_label(dir: &str) -> String {
    if dir == "main" {
        "main (development)".to_string()
    } else {
        dir.to_string()
    }
}

fn write_docs_root_index(out_dir: &Path, current_version_dir: &str) -> Result<()> {
    let versions = docs_versions(out_dir, current_version_dir)?;
    let redirect_dir = versions
        .iter()
        .find(|version| version.dir == "main")
        .or_else(|| versions.iter().find(|version| version.current))
        .map(|version| version.dir.as_str())
        .unwrap_or(current_version_dir);
    let redirect_href = format!("{redirect_dir}/");
    let html = format!(
        r#"<!doctype html><html lang="en"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width, initial-scale=1"><meta http-equiv="refresh" content="0; url={href_attr}"><link rel="canonical" href="{href_attr}"><title>electrode-web docs</title><style>{css}</style></head><body><main><section class="panel"><p class="eyebrow">electrode-web</p><h1>Developer Docs</h1><p>Redirecting to <a href="{href_attr}">{href_text}</a>.</p></section></main><script>window.location.replace({href_js});</script></body></html>"#,
        href_attr = escape_attr(&redirect_href),
        href_text = escape_html(&redirect_href),
        href_js = js_string(&redirect_href),
        css = ROOT_DOCS_CSS,
    );
    write_file(&out_dir.join("index.html"), &html)
}

fn refresh_docs_version_selectors(out_dir: &Path, current_version_dir: &str) -> Result<()> {
    let versions = docs_versions(out_dir, current_version_dir)?;
    for version in &versions {
        let theme_dir = out_dir.join(&version.dir).join("theme");
        if !theme_dir.is_dir() {
            continue;
        }
        let js = render_version_selector_js(&versions, &version.dir);
        for entry in fs::read_dir(&theme_dir)? {
            let path = entry?.path();
            let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
                continue;
            };
            if name.starts_with("version-selector") && name.ends_with(".js") {
                write_file(&path, &js)?;
            }
        }
    }
    Ok(())
}

fn write_docs_version_redirect_aliases(out_dir: &Path, current_version_dir: &str) -> Result<()> {
    let versions = docs_versions(out_dir, current_version_dir)?;
    for source in &versions {
        for target in &versions {
            if source.dir == target.dir {
                continue;
            }
            let target_path = out_dir.join(&source.dir).join(&target.dir);
            fs::create_dir_all(&target_path)?;
            write_file(
                &target_path.join("index.html"),
                &render_redirect_html(&format!("../../{}/", target.dir)),
            )?;
        }
    }
    Ok(())
}

fn render_redirect_html(href: &str) -> String {
    format!(
        r#"<!doctype html><html lang="en"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width, initial-scale=1"><meta http-equiv="refresh" content="0; url={href_attr}"><link rel="canonical" href="{href_attr}"><title>Redirecting</title></head><body><p>Redirecting to <a href="{href_attr}">{href_text}</a>.</p><script>window.location.replace({href_js});</script></body></html>"#,
        href_attr = escape_attr(href),
        href_text = escape_html(href),
        href_js = js_string(href),
    )
}

fn render_version_selector_js(versions: &[DocVersion], current_version_dir: &str) -> String {
    let mut js = String::new();
    js.push_str("(function () {\n");
    js.push_str("  const versions = [\n");
    for version in versions {
        js.push_str("    { dir: ");
        js.push_str(&js_string(&version.dir));
        js.push_str(", label: ");
        js.push_str(&js_string(&version.label));
        js.push_str(" },\n");
    }
    js.push_str("  ];\n");
    js.push_str("  const current = ");
    js.push_str(&js_string(current_version_dir));
    js.push_str(
        r#";
  function docsBaseUrl() {
    const script = document.currentScript || document.querySelector('script[src*="version-selector"]');
    if (!script) {
      return new URL('../', window.location.href);
    }
    const scriptUrl = new URL(script.getAttribute('src'), window.location.href);
    return new URL('../../', scriptUrl);
  }

  function targetUrl(dir) {
    return new URL(dir.replace(/\/+$/, '') + '/', docsBaseUrl()).href;
  }

  function buildSelect() {
    const select = document.createElement('select');
    select.className = 'electrode-version-select';
    select.setAttribute('aria-label', 'Documentation version');
    for (const version of versions) {
      const option = document.createElement('option');
      option.value = version.dir;
      option.textContent = version.label;
      option.selected = version.dir === current;
      select.appendChild(option);
    }
    select.addEventListener('change', () => {
      window.location.href = targetUrl(select.value);
    });
    return select;
  }

  function mountMenu() {
    const menu = document.getElementById('mdbook-menu-bar');
    if (!menu || menu.querySelector('.electrode-version-menu')) {
      return;
    }
    const target = menu.querySelector('.right-buttons') || menu;
    const wrapper = document.createElement('div');
    wrapper.className = 'electrode-version-menu';
    const label = document.createElement('label');
    label.textContent = 'Docs';
    wrapper.appendChild(label);
    wrapper.appendChild(buildSelect());
    target.insertBefore(wrapper, target.firstChild);
  }

  if (document.readyState === 'loading') {
    document.addEventListener('DOMContentLoaded', mountMenu);
  } else {
    mountMenu();
  }
})();
"#,
    );
    js
}

fn normalize_docs_version(version: &str) -> String {
    let version = version.trim();
    if let Some(version) = version.strip_prefix("refs/tags/") {
        return normalize_docs_version(version);
    }
    if let Some(version) = version.strip_prefix("refs/heads/") {
        return normalize_docs_version(version);
    }
    version.strip_prefix('v').unwrap_or(version).to_string()
}

fn docs_dir_name(version: &str) -> String {
    version
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '.' | '-' | '_') {
                ch
            } else {
                '-'
            }
        })
        .collect()
}

fn reset_dir(path: &Path) -> Result<()> {
    if path.exists() {
        fs::remove_dir_all(path)?;
    }
    fs::create_dir_all(path)?;
    Ok(())
}

fn copy_dir_recursive(source: &Path, target: &Path) -> Result<()> {
    fs::create_dir_all(target)?;
    for entry in fs::read_dir(source)? {
        let entry = entry?;
        let source_path = entry.path();
        let target_path = target.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_dir_recursive(&source_path, &target_path)?;
        } else {
            copy_file(&source_path, &target_path)?;
        }
    }
    Ok(())
}

fn copy_file(source: &Path, target: &Path) -> Result<()> {
    if let Some(parent) = target.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::copy(source, target)?;
    Ok(())
}

fn write_file(path: &Path, contents: &str) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, contents)?;
    Ok(())
}

fn run(command: &mut Command) -> Result<()> {
    let status = command.status()?;
    if status.success() {
        Ok(())
    } else {
        fail(format!(
            "command failed with status {status:?}: {command:?}"
        ))
    }
}

fn fail<T>(message: impl Into<String>) -> Result<T> {
    Err(io::Error::other(message.into()).into())
}

fn escape_toml_basic(value: &str) -> String {
    value.replace('\\', "\\\\").replace('"', "\\\"")
}

fn escape_html(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

fn escape_attr(value: &str) -> String {
    escape_html(value).replace('"', "&quot;")
}

fn js_string(value: &str) -> String {
    let mut out = String::from("\"");
    for ch in value.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            _ => out.push(ch),
        }
    }
    out.push('"');
    out
}

const ROOT_DOCS_CSS: &str = r#"
:root {
  color-scheme: light dark;
  font-family: system-ui, sans-serif;
}
body {
  align-items: center;
  background: #18202a;
  color: #f8fafc;
  display: flex;
  margin: 0;
  min-height: 100vh;
}
main {
  margin: 0 auto;
  max-width: 48rem;
  padding: 2rem;
}
.panel {
  border-left: 4px solid #f2b84b;
  padding-left: 1.5rem;
}
.eyebrow {
  color: #f2b84b;
  font-size: .8rem;
  font-weight: 700;
  letter-spacing: .08em;
  text-transform: uppercase;
}
a {
  color: #9cc9ff;
}
"#;
