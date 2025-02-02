use std::collections::{HashMap, HashSet};
use std::env;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use actix_web::body::MessageBody;
use actix_web::dev::ServiceResponse;
use actix_web::http::StatusCode;
use actix_web::middleware::{ErrorHandlerResponse, ErrorHandlers};
use actix_web::{get, web, App, HttpResponse, HttpServer, Result};
use ammonia::Builder;
use chrono::{DateTime, Local};
use html_escape::encode_text;
use humansize::{format_size, BINARY};
use ignore::gitignore::{Gitignore, GitignoreBuilder};
use lazy_static::lazy_static;
use pulldown_cmark::{html, CodeBlockKind, Event, Options, Parser, Tag, TagEnd};
use serde::Serialize;
use syntect::highlighting::ThemeSet;
use syntect::html::highlighted_html_for_string;
use syntect::parsing::SyntaxSet;
use tera::{Context, Tera};
use walkdir::WalkDir;
use zip::write::ExtendedFileOptions;
use zip::{write::FileOptions, ZipWriter};

const DEFAULT_WORKSPACE_ROOT: &str = "/etc/tn3wrepo/Projects";
const MAX_FILE_SIZE: u64 = 10 * 1024 * 1024; // 10MB limit

lazy_static! {
    static ref FAVICON_ICO: Option<Vec<u8>> = {
        let favicon_path = Path::new("favicon.ico");
        fs::read(favicon_path).ok()
    };
    static ref AMMONIA_CODE_BUILDER: Builder<'static> = {
        let mut builder = Builder::new();
        let mut tags = HashSet::new();
        tags.insert("div");
        tags.insert("span");
        tags.insert("pre");
        tags.insert("code");

        let mut tag_attributes = HashMap::new();
        let mut div_attrs = HashSet::new();
        div_attrs.insert("class");
        tag_attributes.insert("div", div_attrs);

        let mut span_attrs = HashSet::new();
        span_attrs.insert("class");
        span_attrs.insert("style");
        tag_attributes.insert("span", span_attrs);

        builder
            .tags(tags)
            .tag_attributes(tag_attributes)
            .clean_content_tags(HashSet::new());
        builder
    };
    static ref AMMONIA_BUILDER: Builder<'static> = {
        let mut builder = Builder::new();
        let mut tags = HashSet::new();
        tags.insert("p");
        tags.insert("br");
        tags.insert("h1");
        tags.insert("h2");
        tags.insert("h3");
        tags.insert("h4");
        tags.insert("h5");
        tags.insert("h6");
        tags.insert("strong");
        tags.insert("em");
        tags.insert("code");
        tags.insert("pre");
        tags.insert("blockquote");
        tags.insert("ul");
        tags.insert("ol");
        tags.insert("li");
        tags.insert("span");
        tags.insert("div");
        tags.insert("img");
        tags.insert("a");
        tags.insert("hr");
        tags.insert("table");
        tags.insert("thead");
        tags.insert("tbody");
        tags.insert("tr");
        tags.insert("th");
        tags.insert("td");
        tags.insert("del");
        tags.insert("input");
        tags.insert("details");
        tags.insert("summary");

        let mut tag_attributes = HashMap::new();
        let mut a_attrs = HashSet::new();
        a_attrs.insert("href");
        a_attrs.insert("title");
        a_attrs.insert("target");
        tag_attributes.insert("a", a_attrs);

        let mut img_attrs = HashSet::new();
        img_attrs.insert("src");
        img_attrs.insert("alt");
        img_attrs.insert("title");
        img_attrs.insert("width");
        img_attrs.insert("height");
        tag_attributes.insert("img", img_attrs);

        let mut p_attrs = HashSet::new();
        p_attrs.insert("align");
        tag_attributes.insert("p", p_attrs);

        let mut h1_attrs = HashSet::new();
        h1_attrs.insert("align");
        tag_attributes.insert("h1", h1_attrs);

        let mut h2_attrs = HashSet::new();
        h2_attrs.insert("align");
        tag_attributes.insert("h2", h2_attrs);

        let mut h3_attrs = HashSet::new();
        h3_attrs.insert("align");
        tag_attributes.insert("h3", h3_attrs);

        let mut h4_attrs = HashSet::new();
        h4_attrs.insert("align");
        tag_attributes.insert("h4", h4_attrs);

        let mut h5_attrs = HashSet::new();
        h5_attrs.insert("align");
        tag_attributes.insert("h5", h5_attrs);

        let mut h6_attrs = HashSet::new();
        h6_attrs.insert("align");
        tag_attributes.insert("h6", h6_attrs);

        let mut input_attrs = HashSet::new();
        input_attrs.insert("type");
        input_attrs.insert("checked");
        input_attrs.insert("disabled");
        tag_attributes.insert("input", input_attrs);

        let mut th_attrs = HashSet::new();
        th_attrs.insert("align");
        tag_attributes.insert("th", th_attrs);

        let mut td_attrs = HashSet::new();
        td_attrs.insert("align");
        tag_attributes.insert("td", td_attrs);

        let mut url_schemes = HashSet::new();
        url_schemes.insert("http");
        url_schemes.insert("https");
        url_schemes.insert("mailto");

        let mut allowed_classes = HashMap::new();
        let mut input_classes = HashSet::new();
        input_classes.insert("task-list-item-checkbox");
        allowed_classes.insert("input", input_classes);

        builder
            .tags(tags)
            .tag_attributes(tag_attributes)
            .link_rel(Some("noopener noreferrer"))
            .url_schemes(url_schemes)
            .allowed_classes(allowed_classes);
        builder
    };
}

struct AppConfig {
    workspace_root: String,
}

#[derive(Serialize)]
struct FileInfo {
    name: String,
    path: String,
    is_dir: bool,
    size: String,
    last_modified: String,
}

#[derive(Serialize)]
struct TemplateData {
    contents: Vec<FileInfo>,
    file_path: Option<String>,
    is_dir: bool,
    dir_contents: Vec<FileInfo>,
    parent_dir: Option<String>,
    workspace_root: String,
    highlighted_code: Option<String>,
    lines_count: Option<usize>,
    file_size: Option<String>,
    last_modified: Option<String>,
    project_name: Option<String>,
    about_content: Option<String>,
    content_source: Option<String>,
    about_sentence: Option<String>,
    tags: Vec<String>,
}

impl TemplateData {
    fn into_context(self) -> Context {
        let mut context = Context::new();
        context.insert("contents", &self.contents);
        context.insert("file_path", &self.file_path);
        context.insert("is_dir", &self.is_dir);
        context.insert("dir_contents", &self.dir_contents);
        context.insert("parent_dir", &self.parent_dir);
        context.insert("workspace_root", &self.workspace_root);
        context.insert("highlighted_code", &self.highlighted_code);
        context.insert("lines_count", &self.lines_count);
        context.insert("file_size", &self.file_size);
        context.insert("last_modified", &self.last_modified);
        context.insert("project_name", &self.project_name);
        context.insert("about_content", &self.about_content);
        context.insert("content_source", &self.content_source);
        context.insert("about_sentence", &self.about_sentence);
        context.insert("tags", &self.tags);
        context
    }
}

struct AppState {
    tera: Tera,
    syntax_set: SyntaxSet,
    theme_set: ThemeSet,
    config: AppConfig,
}

fn get_error_description(status_code: u16) -> (&'static str, &'static str) {
    match status_code {
        400 => ("Bad Request", "The server cannot process the request due to client error."),
        401 => ("Unauthorized", "Authentication is required to access this resource."),
        403 => ("Forbidden", "You don't have permission to access this resource."),
        404 => ("Not Found", "The requested resource could not be found on this server."),
        405 => ("Method Not Allowed", "The request method is not supported for this resource."),
        406 => ("Not Acceptable", "The requested resource cannot generate content according to the Accept headers."),
        408 => ("Request Timeout", "The server timed out waiting for the request."),
        409 => ("Conflict", "The request conflicts with the current state of the server."),
        410 => ("Gone", "The requested resource is no longer available and has been permanently removed."),
        411 => ("Length Required", "The request did not specify the length of its content."),
        412 => ("Precondition Failed", "The server does not meet one of the preconditions in the request."),
        413 => ("Payload Too Large", "The request is larger than the server is willing or able to process."),
        414 => ("URI Too Long", "The URI provided was too long for the server to process."),
        415 => ("Unsupported Media Type", "The request entity has a media type which the server does not support."),
        416 => ("Range Not Satisfiable", "The client has asked for a portion of the file that lies beyond its end."),
        417 => ("Expectation Failed", "The server cannot meet the requirements of the Expect request-header field."),
        418 => ("I'm a teapot", "The server refuses to brew coffee because it is, permanently, a teapot."),
        422 => ("Unprocessable Entity", "The request was well-formed but was unable to be followed due to semantic errors."),
        423 => ("Locked", "The resource that is being accessed is locked."),
        424 => ("Failed Dependency", "The request failed due to failure of a previous request."),
        428 => ("Precondition Required", "The origin server requires the request to be conditional."),
        429 => ("Too Many Requests", "You have sent too many requests in a given amount of time."),
        431 => ("Request Header Fields Too Large", "The server is unwilling to process the request because its header fields are too large."),
        451 => ("Unavailable For Legal Reasons", "The requested resource is unavailable due to legal reasons."),
        500 => ("Internal Server Error", "The server encountered an unexpected condition that prevented it from fulfilling the request."),
        501 => ("Not Implemented", "The server does not support the functionality required to fulfill the request."),
        502 => ("Bad Gateway", "The server received an invalid response from the upstream server."),
        503 => ("Service Unavailable", "The server is currently unable to handle the request due to temporary overloading or maintenance."),
        504 => ("Gateway Timeout", "The server did not receive a timely response from the upstream server."),
        505 => ("HTTP Version Not Supported", "The server does not support the HTTP protocol version used in the request."),
        _ => ("Unexpected Error", "An unexpected error occurred while processing your request."),
    }
}

fn handle_error<B>(res: ServiceResponse<B>) -> Result<ErrorHandlerResponse<B>>
where
    B: MessageBody,
{
    let status_code = res.status().as_u16();
    let (title, description) = get_error_description(status_code);

    let mut context = Context::new();
    context.insert("status_code", &status_code);
    context.insert("title", &title);
    context.insert("description", &description);

    let app_state = res
        .request()
        .app_data::<web::Data<Arc<AppState>>>()
        .unwrap();
    let body = app_state
        .tera
        .render("error.html", &context)
        .unwrap_or_else(|_| format!("Error {} - {}\n{}", status_code, title, description));

    let response = HttpResponse::build(res.status())
        .content_type("text/html; charset=utf-8")
        .body(body);

    Ok(ErrorHandlerResponse::Response(ServiceResponse::new(
        res.request().clone(),
        response.map_into_right_body::<B>(),
    )))
}

fn get_gitignore(project_path: &Path) -> Option<Gitignore> {
    let gitignore_path = project_path.join(".gitignore");
    if !gitignore_path.exists() {
        return None;
    }

    let mut builder = GitignoreBuilder::new(project_path);
    match fs::read_to_string(&gitignore_path) {
        Ok(content) => {
            for line in content.lines() {
                builder.add_line(None, line).ok()?;
            }
            builder.build().ok()
        }
        Err(_) => None,
    }
}

fn is_symlink(path: &Path) -> bool {
    path.read_link().is_ok()
}

fn is_path_allowed(path: &Path, check_gitignore: bool, workspace_root: &str) -> bool {
    if !path.exists() {
        return false;
    }

    if is_symlink(path) {
        return false;
    }

    let canonical_path = match path.canonicalize() {
        Ok(p) => p,
        Err(_) => return false,
    };

    let canonical_workspace = match Path::new(workspace_root).canonicalize() {
        Ok(p) => p,
        Err(_) => return false,
    };

    if !canonical_path.starts_with(&canonical_workspace) {
        return false;
    }

    if canonical_path == canonical_workspace {
        return true;
    }

    let rel_path = match canonical_path.strip_prefix(&canonical_workspace) {
        Ok(p) => p,
        Err(_) => return false,
    };

    let project_root = canonical_workspace.join(
        rel_path
            .components()
            .next()
            .unwrap_or_else(|| std::path::Component::Normal("".as_ref())),
    );

    if !project_root.is_dir() {
        return false;
    }

    if rel_path.components().any(|c| {
        let name = c.as_os_str().to_string_lossy();
        (name.starts_with('.') && name != ".gitignore")
            || (name == "ABOUT" && !path.ends_with("ABOUT"))
    }) {
        return false;
    }

    if canonical_path == project_root {
        return true;
    }

    if check_gitignore {
        if let Some(gitignore) = get_gitignore(&project_root) {
            let rel_to_project = canonical_path
                .strip_prefix(&project_root)
                .unwrap_or(rel_path);
            if gitignore.matched(rel_to_project, false).is_ignore() {
                return false;
            }
        }
    }

    true
}

fn get_file_info(path: &Path, workspace_root: &str) -> Option<FileInfo> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(m) => m,
        Err(_) => return None,
    };

    if metadata.file_type().is_symlink() {
        return None;
    }

    if !metadata.is_dir() && metadata.len() > MAX_FILE_SIZE {
        return None;
    }

    let name = match path.file_name() {
        Some(name) => name.to_string_lossy().into_owned(),
        None => return None,
    };

    let canonical_path = match path.canonicalize() {
        Ok(p) => p,
        Err(_) => return None,
    };

    let canonical_workspace = match Path::new(workspace_root).canonicalize() {
        Ok(p) => p,
        Err(_) => return None,
    };

    let rel_path = match canonical_path.strip_prefix(&canonical_workspace) {
        Ok(p) => p.to_string_lossy().into_owned(),
        Err(_) => return None,
    };

    let last_modified = match metadata.modified() {
        Ok(time) => DateTime::<Local>::from(time)
            .format("%b %d, %Y %H:%M")
            .to_string(),
        Err(_) => return None,
    };

    Some(FileInfo {
        name,
        path: rel_path,
        is_dir: metadata.is_dir(),
        size: format_size(metadata.len(), BINARY),
        last_modified,
    })
}

fn is_binary_file(path: &Path) -> bool {
    if let Ok(content) = fs::read(path) {
        return content.iter().take(1024).any(|&byte| byte == 0);
    }
    true
}

fn highlight_code(
    path: &Path,
    content: &str,
    ss: &SyntaxSet,
    ts: &ThemeSet,
    with_line_numbers: bool,
) -> String {
    let extension = path.extension().and_then(|ext| ext.to_str()).unwrap_or("");

    let syntax = ss
        .find_syntax_by_extension(extension)
        .unwrap_or_else(|| ss.find_syntax_plain_text());

    let light_theme = &ts.themes["InspiredGitHub"];
    let dark_theme = &ts.themes["base16-eighties.dark"];

    let process_html = |html: String| {
        if !with_line_numbers {
            return html;
        }

        let lines: Vec<&str> = content.lines().collect();
        let line_count = lines.len();
        let gutter_width = format!("{}", line_count).len();

        let line_numbers = (1..=line_count)
            .map(|i| {
                format!(
                    "<span class=\"line-number\">{:>width$}</span>",
                    i,
                    width = gutter_width
                )
            })
            .collect::<Vec<_>>()
            .join("\n");

        format!(
            r#"<div class="line-numbers">{}</div><div class="code-content">{}</div>"#,
            line_numbers, html
        )
    };

    let dark_html = highlighted_html_for_string(content, ss, syntax, dark_theme)
        .map(|html| process_html(html))
        .unwrap_or_else(|_| encode_text(&content).to_string());

    let light_html = highlighted_html_for_string(content, ss, syntax, light_theme)
        .map(|html| process_html(html))
        .unwrap_or_else(|_| encode_text(&content).to_string());

    let wrap_code = |html: &str| {
        if with_line_numbers {
            format!(r#"<div class="code-with-lines">{}</div>"#, html)
        } else {
            html.to_string()
        }
    };

    format!(
        r#"<div class="dark-code">{}</div><div class="light-code">{}</div>"#,
        wrap_code(&dark_html),
        wrap_code(&light_html)
    )
}

fn render_markdown(content: &str, base_path: &str, ss: &SyntaxSet, ts: &ThemeSet) -> String {
    let mut options = Options::empty();
    options.insert(Options::ENABLE_TABLES);
    options.insert(Options::ENABLE_FOOTNOTES);
    options.insert(Options::ENABLE_STRIKETHROUGH);
    options.insert(Options::ENABLE_TASKLISTS);

    let parser = Parser::new_ext(content, options);

    let mut html_output = String::new();
    let mut in_code_block = false;
    let mut current_code = String::new();
    let mut current_lang = String::new();
    let mut code_blocks = Vec::new();
    let placeholder_prefix = "__CODE_BLOCK_PLACEHOLDER_";

    for event in parser {
        match event {
            Event::Start(Tag::CodeBlock(CodeBlockKind::Fenced(lang))) => {
                in_code_block = true;
                current_lang = lang.to_string();
                current_code.clear();
                continue;
            }
            Event::End(TagEnd::CodeBlock) if in_code_block => {
                let extension = match current_lang.as_str() {
                    "rust" | "rs" => "rs",
                    "c" => "c",
                    "cpp" | "c++" | "cxx" | "cc" => "cpp",
                    "h" | "hpp" | "hxx" | "hh" => "h",
                    "asm" | "s" => "asm",
                    "javascript" | "js" | "jsx" => "js",
                    "typescript" | "ts" | "tsx" => "ts",
                    "html" | "htm" | "xhtml" => "html",
                    "css" | "scss" | "sass" | "less" => "css",
                    "php" => "php",
                    "vue" => "vue",
                    "svelte" => "svelte",
                    "python" | "py" | "pyw" | "pyx" => "py",
                    "ruby" | "rb" | "rbw" => "rb",
                    "perl" | "pl" | "pm" => "pl",
                    "lua" => "lua",
                    "tcl" => "tcl",
                    "java" => "java",
                    "kotlin" | "kt" => "kt",
                    "groovy" => "groovy",
                    "scala" => "scala",
                    "clojure" | "clj" => "clj",
                    "cs" | "csharp" => "cs",
                    "fs" | "fsharp" => "fs",
                    "vb" => "vb",
                    "shell" | "sh" | "bash" | "zsh" | "fish" => "sh",
                    "powershell" | "ps1" => "ps1",
                    "batch" | "bat" | "cmd" => "bat",
                    "go" | "golang" => "go",
                    "swift" => "swift",
                    "r" => "r",
                    "matlab" | "m" => "matlab",
                    "haskell" | "hs" => "hs",
                    "elixir" | "ex" | "exs" => "ex",
                    "erlang" | "erl" => "erl",
                    "ocaml" | "ml" => "ml",
                    "lisp" | "el" => "lisp",
                    "scheme" | "scm" => "scm",
                    "dart" => "dart",
                    "d" => "d",
                    "json" => "json",
                    "yaml" | "yml" => "yaml",
                    "toml" => "toml",
                    "xml" => "xml",
                    "sql" => "sql",
                    "graphql" | "gql" => "graphql",
                    "protobuf" | "proto" => "proto",
                    "markdown" | "md" => "md",
                    "tex" | "latex" => "tex",
                    "rst" => "rst",
                    "asciidoc" | "adoc" => "adoc",
                    _ => "txt",
                };

                let temp_path_str = format!("temp_{}.{}", code_blocks.len(), extension);
                let temp_path = Path::new(&temp_path_str);
                let highlighted = highlight_code(temp_path, &current_code, ss, ts, false);
                let clean_highlighted = AMMONIA_CODE_BUILDER.clean(&highlighted).to_string();
                let placeholder = format!("{}{}_END", placeholder_prefix, code_blocks.len());

                code_blocks.push(clean_highlighted);
                html_output.push_str(&placeholder);

                in_code_block = false;
                current_code.clear();
                current_lang.clear();
                continue;
            }
            Event::Text(text) if in_code_block => {
                current_code.push_str(&text);
                continue;
            }
            _ => {}
        }

        html::push_html(&mut html_output, std::iter::once(event));
    }

    let clean_html = AMMONIA_BUILDER.clean(&html_output).to_string();

    let mut final_html = clean_html;
    for (i, code) in code_blocks.iter().enumerate() {
        let placeholder = format!("{}{}_END", placeholder_prefix, i);
        final_html = final_html.replace(&placeholder, code);
    }

    final_html
        .replace("src=\"./", &format!("src=\"/{}/", base_path))
        .replace("href=\"./", &format!("href=\"/{}/", base_path))
}

fn parse_about_file(path: &Path) -> Option<(Vec<String>, Option<String>)> {
    let content = fs::read_to_string(path).ok()?;
    let lines: Vec<&str> = content.lines().collect();

    let tags: Vec<String> = lines
        .iter()
        .filter(|line| line.starts_with('#'))
        .map(|line| line.trim_start_matches('#').trim().to_string())
        .collect();

    let about_sentence = lines
        .iter()
        .find(|line| !line.starts_with('#') && !line.trim().is_empty())
        .map(|line| line.trim().to_string());

    Some((tags, about_sentence))
}

fn get_project_content(
    project_path: &Path,
    workspace_root: &str,
    ss: &SyntaxSet,
    ts: &ThemeSet,
) -> (Option<String>, Vec<String>, Option<String>, Option<String>) {
    let mut content = None;
    let mut tags = Vec::new();
    let mut source_file = None;
    let mut about_sentence = None;

    let readme_path = project_path.join("README.md");
    if readme_path.exists() && is_path_allowed(&readme_path, true, workspace_root) {
        if let Ok(readme_content) = fs::read_to_string(&readme_path) {
            content = Some(render_markdown(&readme_content, workspace_root, ss, ts));
            source_file = Some("README.md".to_string());
        }
    }

    let about_path = project_path.join("ABOUT");
    if about_path.exists() {
        if let Some((about_tags, about_sent)) = parse_about_file(&about_path) {
            if content.is_none() {
                content = about_sent
                    .clone()
                    .map(|s| render_markdown(&s, workspace_root, ss, ts));
                source_file = Some("ABOUT".to_string());
            }
            tags = about_tags;
            about_sentence = about_sent;
        }
    }

    (content, tags, source_file, about_sentence)
}

fn create_zip_file(directory_path: &Path, workspace_root: &str) -> Option<Vec<u8>> {
    let buffer = Vec::new();
    let cursor = std::io::Cursor::new(buffer);
    let mut zip = ZipWriter::new(cursor);
    let options: FileOptions<ExtendedFileOptions> = FileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated)
        .unix_permissions(0o755);

    let walk = WalkDir::new(directory_path).into_iter();
    for entry in walk.filter_map(|e| e.ok()) {
        let path = entry.path();

        if path.file_name().map_or(false, |n| n == "ABOUT") {
            continue;
        }

        if !is_path_allowed(path, true, workspace_root) {
            continue;
        }

        let name = path.strip_prefix(directory_path).ok()?;
        if name.as_os_str().is_empty() {
            continue;
        }

        if path.is_file() {
            zip.start_file(name.to_string_lossy(), options.clone())
                .ok()?;
            let content = fs::read(path).ok()?;
            zip.write_all(&content).ok()?;
        }
    }

    zip.finish().ok().map(|cursor| cursor.into_inner())
}

fn is_project_root(path: &Path, workspace_root: &str) -> bool {
    let canonical_path = match path.canonicalize() {
        Ok(p) => p,
        Err(_) => return false,
    };

    let canonical_workspace = match Path::new(workspace_root).canonicalize() {
        Ok(p) => p,
        Err(_) => return false,
    };

    let rel_path = match canonical_path.strip_prefix(&canonical_workspace) {
        Ok(p) => p,
        Err(_) => return false,
    };

    rel_path.components().count() == 1 && canonical_path.is_dir()
}

fn get_directory_contents(
    path: &Path,
    check_gitignore: bool,
    workspace_root: &str,
) -> Vec<FileInfo> {
    let mut contents: Vec<FileInfo> = if path == Path::new(workspace_root) {
        WalkDir::new(path)
            .min_depth(1)
            .max_depth(1)
            .into_iter()
            .filter_map(|entry| entry.ok())
            .filter(|entry| entry.path().is_dir())
            .filter_map(|entry| get_file_info(entry.path(), workspace_root))
            .collect()
    } else {
        WalkDir::new(path)
            .min_depth(1)
            .max_depth(1)
            .into_iter()
            .filter_map(|entry| entry.ok())
            .filter(|entry| is_path_allowed(entry.path(), check_gitignore, workspace_root))
            .filter_map(|entry| get_file_info(entry.path(), workspace_root))
            .collect()
    };

    contents.sort_by(|a, b| match (a.is_dir, b.is_dir) {
        (true, true) | (false, false) => a.name.to_lowercase().cmp(&b.name.to_lowercase()),
        (true, false) => std::cmp::Ordering::Less,
        (false, true) => std::cmp::Ordering::Greater,
    });

    contents
}

#[get("/")]
async fn index(data: web::Data<Arc<AppState>>) -> Result<HttpResponse> {
    let workspace_root = &data.config.workspace_root;
    let mut context = TemplateData {
        contents: Vec::new(),
        file_path: None,
        is_dir: true,
        dir_contents: Vec::new(),
        parent_dir: None,
        workspace_root: workspace_root.clone(),
        highlighted_code: None,
        lines_count: None,
        file_size: None,
        last_modified: None,
        project_name: None,
        about_content: None,
        content_source: None,
        about_sentence: None,
        tags: Vec::new(),
    };

    context.contents = get_directory_contents(Path::new(workspace_root), false, workspace_root);

    let body = data
        .tera
        .render("index.html", &context.into_context())
        .map_err(|e| {
            eprintln!("Template error: {}", e);
            actix_web::error::ErrorInternalServerError("Template error")
        })?;

    return Ok(HttpResponse::Ok()
        .content_type("text/html")
        .insert_header(("Cache-Control", "public, max-age=86400"))
        .body(body));
}

#[get("/ping")]
async fn ping() -> Result<HttpResponse> {
    Ok(HttpResponse::Ok()
        .insert_header(("Cache-Control", "no-store"))
        .body("pong"))
}

#[get("/favicon.ico")]
async fn favicon_ico() -> Result<HttpResponse> {
    if let Some(content) = FAVICON_ICO.as_ref() {
        Ok(HttpResponse::Ok()
            .content_type("image/x-icon")
            .insert_header(("Cache-Control", "public, max-age=86400"))
            .body(content.clone()))
    } else {
        Ok(HttpResponse::NotFound().finish())
    }
}

#[get("/robots.txt")]
async fn robots_txt() -> Result<HttpResponse> {
    Ok(HttpResponse::Ok()
        .content_type("text/plain")
        .insert_header(("Cache-Control", "public, max-age=86400"))
        .body("User-agent: *\nAllow: /\n"))
}

#[get("/download/{path:.*}")]
async fn download_file(
    path: web::Path<String>,
    data: web::Data<Arc<AppState>>,
) -> Result<HttpResponse> {
    let path_str = path.into_inner();
    let workspace_root = &data.config.workspace_root;
    let file_path = PathBuf::from(workspace_root).join(&path_str);

    let canonical_path = match file_path.canonicalize() {
        Ok(p) => p,
        Err(_) => return Err(actix_web::error::ErrorNotFound("File not found")),
    };

    if !is_path_allowed(&canonical_path, true, workspace_root) {
        return Err(actix_web::error::ErrorNotFound("File not found"));
    }

    if is_symlink(&canonical_path) {
        return Err(actix_web::error::ErrorForbidden("Access denied"));
    }

    let metadata = match fs::symlink_metadata(&canonical_path) {
        Ok(m) => m,
        Err(_) => return Err(actix_web::error::ErrorNotFound("File not found")),
    };

    if metadata.len() > MAX_FILE_SIZE {
        return Err(actix_web::error::ErrorForbidden("File too large"));
    }

    if canonical_path.is_dir() {
        if let Some(zip_data) = create_zip_file(&canonical_path, &workspace_root) {
            let filename = format!(
                "{}.zip",
                canonical_path.file_name().unwrap().to_string_lossy()
            );
            return Ok(HttpResponse::Ok()
                .content_type("application/zip")
                .insert_header(("Cache-Control", "public, max-age=86400"))
                .insert_header(("X-Content-Type-Options", "nosniff"))
                .insert_header((
                    "Content-Disposition",
                    format!("attachment; filename=\"{}\"", encode_text(&filename)),
                ))
                .body(zip_data));
        }
        return Err(actix_web::error::ErrorInternalServerError(
            "Failed to create zip",
        ));
    }

    let filename = canonical_path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("download");

    let content_type = match canonical_path.extension().and_then(|ext| ext.to_str()) {
        Some("txt") => "text/plain",
        Some("html") | Some("htm") => "text/plain",
        Some("css") => "text/css",
        Some("js") => "text/javascript",
        Some("json") => "application/json",
        Some("png") => "image/png",
        Some("jpg") | Some("jpeg") => "image/jpeg",
        Some("gif") => "image/gif",
        Some("pdf") => "application/pdf",
        Some("zip") => "application/zip",
        Some("md") => "text/markdown",
        Some("rs") => "text/plain",
        Some("py") => "text/plain",
        Some("go") => "text/plain",
        Some("java") => "text/plain",
        Some("c") | Some("cpp") | Some("h") | Some("hpp") => "text/plain",
        _ => "application/octet-stream",
    };

    let file_content =
        fs::read(&canonical_path).map_err(|_| actix_web::error::ErrorNotFound("File not found"))?;

    Ok(HttpResponse::Ok()
        .content_type(content_type)
        .insert_header(("Cache-Control", "public, max-age=86400"))
        .insert_header(("X-Content-Type-Options", "nosniff"))
        .insert_header((
            "Content-Disposition",
            format!("attachment; filename=\"{}\"", encode_text(filename)),
        ))
        .body(file_content))
}

#[get("/{path:.*}")]
async fn view_path(
    path: web::Path<String>,
    data: web::Data<Arc<AppState>>,
) -> Result<HttpResponse> {
    let path_str = path.into_inner();
    let workspace_root = &data.config.workspace_root;

    if path_str.is_empty() {
        let mut context = TemplateData {
            contents: Vec::new(),
            file_path: None,
            is_dir: true,
            dir_contents: Vec::new(),
            parent_dir: None,
            workspace_root: workspace_root.clone(),
            highlighted_code: None,
            lines_count: None,
            file_size: None,
            last_modified: None,
            project_name: None,
            about_content: None,
            content_source: None,
            about_sentence: None,
            tags: Vec::new(),
        };

        context.contents = get_directory_contents(Path::new(workspace_root), false, workspace_root);

        let body = data
            .tera
            .render("index.html", &context.into_context())
            .map_err(|_| actix_web::error::ErrorInternalServerError("Internal server error"))?;

        return Ok(HttpResponse::Ok()
            .content_type("text/html; charset=utf-8")
            .insert_header(("Cache-Control", "public, max-age=86400"))
            .insert_header(("X-Content-Type-Options", "nosniff"))
            .insert_header(("X-Frame-Options", "DENY"))
            .insert_header(("X-XSS-Protection", "1; mode=block"))
            .body(body));
    }

    let file_path = PathBuf::from(workspace_root).join(&path_str);

    let canonical_path = match file_path.canonicalize() {
        Ok(p) => p,
        Err(err) => {
            println!("Error: {}", err);
            return Err(actix_web::error::ErrorNotFound("Path not found"));
        }
    };

    if !is_path_allowed(&canonical_path, true, workspace_root) {
        return Err(actix_web::error::ErrorNotFound("Path not found"));
    }

    if is_symlink(&canonical_path) {
        return Err(actix_web::error::ErrorForbidden("Access denied"));
    }

    let current_dir = if canonical_path.is_dir() {
        canonical_path.clone()
    } else {
        canonical_path
            .parent()
            .unwrap_or(&canonical_path)
            .to_path_buf()
    };

    let dir_contents = get_directory_contents(&current_dir, true, workspace_root);

    let parent_dir = if let (Ok(canonical_current), Ok(canonical_workspace)) = (
        current_dir.canonicalize(),
        Path::new(workspace_root).canonicalize(),
    ) {
        if canonical_current == canonical_workspace {
            None
        } else {
            canonical_current
                .strip_prefix(&canonical_workspace)
                .ok()
                .and_then(|rel_path| rel_path.parent())
                .map(|p| p.to_string_lossy().into_owned())
        }
    } else {
        None
    };

    let mut context = TemplateData {
        contents: Vec::new(),
        file_path: Some(path_str),
        is_dir: canonical_path.is_dir(),
        dir_contents,
        parent_dir,
        workspace_root: workspace_root.clone(),
        highlighted_code: None,
        lines_count: None,
        file_size: None,
        last_modified: None,
        project_name: None,
        about_content: None,
        content_source: None,
        about_sentence: None,
        tags: Vec::new(),
    };

    if !canonical_path.is_dir() {
        let metadata = match fs::symlink_metadata(&canonical_path) {
            Ok(m) => m,
            Err(_) => return Err(actix_web::error::ErrorNotFound("File not found")),
        };

        if metadata.len() > MAX_FILE_SIZE {
            return Err(actix_web::error::ErrorForbidden("File too large"));
        }

        if is_binary_file(&canonical_path) {
            return Err(actix_web::error::ErrorBadRequest("Binary file"));
        }

        let content = fs::read_to_string(&canonical_path)
            .map_err(|_| actix_web::error::ErrorNotFound("File not found"))?;

        let file_info = get_file_info(&canonical_path, workspace_root)
            .ok_or_else(|| actix_web::error::ErrorNotFound("File not found"))?;

        let highlighted_code = highlight_code(
            &canonical_path,
            &content,
            &data.syntax_set,
            &data.theme_set,
            true,
        );

        context.highlighted_code = Some(AMMONIA_CODE_BUILDER.clean(&highlighted_code).to_string());
        context.lines_count = Some(content.lines().count());
        context.file_size = Some(file_info.size);
        context.last_modified = Some(file_info.last_modified);

        let body = data
            .tera
            .render("code_view.html", &context.into_context())
            .map_err(|_| actix_web::error::ErrorInternalServerError("Internal server error"))?;

        return Ok(HttpResponse::Ok()
            .content_type("text/html; charset=utf-8")
            .insert_header(("Cache-Control", "public, max-age=86400"))
            .insert_header(("X-Content-Type-Options", "nosniff"))
            .insert_header(("X-Frame-Options", "DENY"))
            .insert_header(("X-XSS-Protection", "1; mode=block"))
            .body(body));
    }

    context.contents = get_directory_contents(&canonical_path, true, workspace_root);

    if !is_project_root(&canonical_path, workspace_root) {
        let body = data
            .tera
            .render("code_view.html", &context.into_context())
            .map_err(|_| actix_web::error::ErrorInternalServerError("Internal server error"))?;

        return Ok(HttpResponse::Ok()
            .content_type("text/html; charset=utf-8")
            .insert_header(("Cache-Control", "public, max-age=86400"))
            .insert_header(("X-Content-Type-Options", "nosniff"))
            .insert_header(("X-Frame-Options", "DENY"))
            .insert_header(("X-XSS-Protection", "1; mode=block"))
            .body(body));
    }

    let (content, tags, source_file, about_sentence) = get_project_content(
        &canonical_path,
        &workspace_root,
        &data.syntax_set,
        &data.theme_set,
    );
    context.project_name = Some(
        canonical_path
            .file_name()
            .unwrap()
            .to_string_lossy()
            .into_owned(),
    );
    context.about_content = content.map(|c| c.to_string());
    context.content_source = source_file;
    context.about_sentence = about_sentence.map(|s| encode_text(&s).to_string());
    context.tags = tags
        .into_iter()
        .map(|t| encode_text(&t).to_string())
        .collect();

    let body = data
        .tera
        .render("repo_view.html", &context.into_context())
        .map_err(|_| actix_web::error::ErrorInternalServerError("Internal server error"))?;

    Ok(HttpResponse::Ok()
        .content_type("text/html; charset=utf-8")
        .insert_header(("Cache-Control", "public, max-age=86400"))
        .insert_header(("X-Content-Type-Options", "nosniff"))
        .insert_header(("X-Frame-Options", "DENY"))
        .insert_header(("X-XSS-Protection", "1; mode=block"))
        .body(body))
}

#[actix_web::main]
async fn main() -> std::io::Result<()> {
    let args: Vec<String> = env::args().collect();
    let workspace_root: PathBuf = if args.len() > 1 {
        PathBuf::from(&args[1])
    } else {
        PathBuf::from(DEFAULT_WORKSPACE_ROOT)
    };

    if !workspace_root.exists() {
        fs::create_dir_all(&workspace_root).map_err(|e| {
            eprintln!("Failed to create workspace directory: {}", e);
            std::io::Error::new(
                std::io::ErrorKind::Other,
                "Failed to create workspace directory",
            )
        })?;
    }

    if !workspace_root.is_dir() {
        eprintln!("Error: {} is not a directory", workspace_root.display());
        eprintln!("Usage: syntaxia <path-to-projects>");
        std::process::exit(1);
    }

    let mut tera = Tera::default();
    tera.add_template_files(vec![
        ("templates/index.html", Some("index.html")),
        ("templates/code_view.html", Some("code_view.html")),
        ("templates/repo_view.html", Some("repo_view.html")),
        ("templates/error.html", Some("error.html")),
    ])
    .unwrap();

    let syntax_set = SyntaxSet::load_defaults_newlines();
    let theme_set = ThemeSet::load_defaults();

    let config = AppConfig {
        workspace_root: workspace_root.to_string_lossy().into_owned(),
    };

    let app_state = Arc::new(AppState {
        tera,
        syntax_set,
        theme_set,
        config,
    });

    HttpServer::new(move || {
        App::new()
            .app_data(web::Data::new(app_state.clone()))
            .wrap(
                actix_web::middleware::DefaultHeaders::new()
                    .add((
                        "Strict-Transport-Security",
                        "max-age=31536000; includeSubDomains".to_string(),
                    ))
                    .add((
                        "Content-Security-Policy",
                        "default-src 'self'; \
                         script-src 'self' 'unsafe-inline' 'unsafe-eval'; \
                         style-src 'self' 'unsafe-inline' https://cdnjs.cloudflare.com; \
                         font-src 'self' https://cdnjs.cloudflare.com; \
                         img-src 'self' data: https:; \
                         connect-src 'self';"
                            .to_string(),
                    ))
                    .add((
                        "Referrer-Policy",
                        "strict-origin-when-cross-origin".to_string(),
                    )),
            )
            .wrap(
                ErrorHandlers::new()
                    .handler(StatusCode::BAD_REQUEST, handle_error)
                    .handler(StatusCode::UNAUTHORIZED, handle_error)
                    .handler(StatusCode::FORBIDDEN, handle_error)
                    .handler(StatusCode::NOT_FOUND, handle_error)
                    .handler(StatusCode::METHOD_NOT_ALLOWED, handle_error)
                    .handler(StatusCode::NOT_ACCEPTABLE, handle_error)
                    .handler(StatusCode::REQUEST_TIMEOUT, handle_error)
                    .handler(StatusCode::CONFLICT, handle_error)
                    .handler(StatusCode::GONE, handle_error)
                    .handler(StatusCode::LENGTH_REQUIRED, handle_error)
                    .handler(StatusCode::PRECONDITION_FAILED, handle_error)
                    .handler(StatusCode::PAYLOAD_TOO_LARGE, handle_error)
                    .handler(StatusCode::URI_TOO_LONG, handle_error)
                    .handler(StatusCode::UNSUPPORTED_MEDIA_TYPE, handle_error)
                    .handler(StatusCode::RANGE_NOT_SATISFIABLE, handle_error)
                    .handler(StatusCode::EXPECTATION_FAILED, handle_error)
                    .handler(StatusCode::IM_A_TEAPOT, handle_error)
                    .handler(StatusCode::UNPROCESSABLE_ENTITY, handle_error)
                    .handler(StatusCode::LOCKED, handle_error)
                    .handler(StatusCode::FAILED_DEPENDENCY, handle_error)
                    .handler(StatusCode::PRECONDITION_REQUIRED, handle_error)
                    .handler(StatusCode::TOO_MANY_REQUESTS, handle_error)
                    .handler(StatusCode::REQUEST_HEADER_FIELDS_TOO_LARGE, handle_error)
                    .handler(StatusCode::UNAVAILABLE_FOR_LEGAL_REASONS, handle_error)
                    .handler(StatusCode::INTERNAL_SERVER_ERROR, handle_error)
                    .handler(StatusCode::NOT_IMPLEMENTED, handle_error)
                    .handler(StatusCode::BAD_GATEWAY, handle_error)
                    .handler(StatusCode::SERVICE_UNAVAILABLE, handle_error)
                    .handler(StatusCode::GATEWAY_TIMEOUT, handle_error)
                    .handler(StatusCode::HTTP_VERSION_NOT_SUPPORTED, handle_error),
            )
            .service(index)
            .service(ping)
            .service(favicon_ico)
            .service(robots_txt)
            .service(download_file)
            .service(view_path)
    })
    .bind(("127.0.0.1", 8201))?
    .workers(16)
    .run()
    .await
}
