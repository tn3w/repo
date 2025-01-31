use std::env;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::collections::{HashMap, HashSet};

use actix_web::{get, web, App, HttpResponse, HttpServer, Result};
use chrono::{DateTime, Local};
use humansize::{format_size, BINARY};
use ignore::gitignore::{Gitignore, GitignoreBuilder};
use pulldown_cmark::{html, Options, Parser};
use serde::Serialize;
use syntect::highlighting::ThemeSet;
use syntect::html::highlighted_html_for_string;
use syntect::parsing::SyntaxSet;
use tera::{Context, Tera};
use walkdir::WalkDir;
use zip::write::ExtendedFileOptions;
use zip::{write::FileOptions, ZipWriter};
use html_escape::encode_text;
use ammonia::Builder;
use lazy_static::lazy_static;

const DEFAULT_WORKSPACE_ROOT: &str = "/etc/tn3wrepo/Projects";
const MAX_FILE_SIZE: u64 = 10 * 1024 * 1024; // 10MB limit

lazy_static! {
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

        let mut tag_attributes = HashMap::new();
        let mut a_attrs = HashSet::new();
        a_attrs.insert("href");
        a_attrs.insert("title");
        tag_attributes.insert("a", a_attrs);

        let mut code_attrs = HashSet::new();
        code_attrs.insert("class");
        code_attrs.insert("style");
        tag_attributes.insert("code", code_attrs);

        let mut span_attrs = HashSet::new();
        span_attrs.insert("class");
        span_attrs.insert("style");
        tag_attributes.insert("span", span_attrs);

        let mut div_attrs = HashSet::new();
        div_attrs.insert("class");
        div_attrs.insert("style");
        tag_attributes.insert("div", div_attrs);

        let mut pre_attrs = HashSet::new();
        pre_attrs.insert("class");
        pre_attrs.insert("style");
        tag_attributes.insert("pre", pre_attrs);

        let mut url_schemes = HashSet::new();
        url_schemes.insert("http");
        url_schemes.insert("https");
        url_schemes.insert("mailto");

        let mut generic_attributes = HashSet::new();
        generic_attributes.insert("style");

        builder
            .tags(tags)
            .tag_attributes(tag_attributes)
            .link_rel(Some("noopener noreferrer"))
            .url_schemes(url_schemes)
            .generic_attributes(generic_attributes);
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
    highlighted_dark_code: Option<String>,
    highlighted_light_code: Option<String>,
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
        context.insert("highlighted_dark_code", &self.highlighted_dark_code);
        context.insert("highlighted_light_code", &self.highlighted_light_code);
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
        (name.starts_with('.') && name != ".gitignore") || 
        (name == "ABOUT" && !path.ends_with("ABOUT"))
    }) {
        return false;
    }

    if canonical_path == project_root {
        return true;
    }

    if check_gitignore {
        if let Some(gitignore) = get_gitignore(&project_root) {
            let rel_to_project = canonical_path.strip_prefix(&project_root).unwrap_or(rel_path);
            if gitignore.matched(rel_to_project, false).is_ignore() {
                return false;
            }
        }
    }

    true
}

fn get_file_info(path: &Path, workspace_root: &str) -> Option<FileInfo> {
    // Get metadata, but don't follow symlinks
    let metadata = match fs::symlink_metadata(path) {
        Ok(m) => m,
        Err(_) => return None,
    };
    
    // Skip symlinks
    if metadata.file_type().is_symlink() {
        return None;
    }

    // Skip files larger than the limit
    if !metadata.is_dir() && metadata.len() > MAX_FILE_SIZE {
        return None;
    }

    // Get the filename, return None if we can't get it
    let name = match path.file_name() {
        Some(name) => name.to_string_lossy().into_owned(),
        None => return None,
    };

    // Convert both paths to canonical form for reliable path operations
    let canonical_path = match path.canonicalize() {
        Ok(p) => p,
        Err(_) => return None,
    };

    let canonical_workspace = match Path::new(workspace_root).canonicalize() {
        Ok(p) => p,
        Err(_) => return None,
    };

    // Get relative path from workspace root
    let rel_path = match canonical_path.strip_prefix(&canonical_workspace) {
        Ok(p) => p.to_string_lossy().into_owned(),
        Err(_) => return None,
    };

    // Get last modified time
    let last_modified = match metadata.modified() {
        Ok(time) => DateTime::<Local>::from(time).format("%b %d, %Y %H:%M").to_string(),
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

#[derive(Debug)]
struct ThemedCode {
    light: String,
    dark: String,
}

fn highlight_code(path: &Path, content: &str, ss: &SyntaxSet, ts: &ThemeSet) -> ThemedCode {
    let extension = path.extension().and_then(|ext| ext.to_str()).unwrap_or("");

    let syntax = ss
        .find_syntax_by_extension(extension)
        .unwrap_or_else(|| ss.find_syntax_plain_text());

    let light_theme = &ts.themes["InspiredGitHub"];
    let dark_theme = &ts.themes["base16-eighties.dark"];

    let process_html = |html: String| {
        let clean_html = AMMONIA_BUILDER.clean(&html).to_string();

        let html = clean_html.replace(r#"<pre style="background-color:#2b303b;">"#, "<pre>");

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

    let light_html = highlighted_html_for_string(content, ss, syntax, light_theme)
        .map(|html| process_html(html))
        .unwrap_or_else(|_|encode_text(&content).to_string());

    let dark_html = highlighted_html_for_string(content, ss, syntax, dark_theme)
        .map(|html| process_html(html))
        .unwrap_or_else(|_| encode_text(&content).to_string());

    ThemedCode {
        light: light_html,
        dark: dark_html,
    }
}

fn render_markdown(content: &str, base_path: &str) -> String {
    let mut options = Options::empty();
    options.insert(Options::ENABLE_TABLES);
    options.insert(Options::ENABLE_FOOTNOTES);
    options.insert(Options::ENABLE_STRIKETHROUGH);
    options.insert(Options::ENABLE_TASKLISTS);

    let parser = Parser::new_ext(content, options);
    
    // Convert to HTML first
    let mut html_output = String::new();
    html::push_html(&mut html_output, parser);

    // Clean the HTML output with ammonia to only allow safe elements and attributes
    let clean_html = AMMONIA_BUILDER.clean(&html_output).to_string();

    // Fix relative URLs
    clean_html
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
) -> (Option<String>, Vec<String>, Option<String>, Option<String>) {
    let mut content = None;
    let mut tags = Vec::new();
    let mut source_file = None;
    let mut about_sentence = None;

    let readme_path = project_path.join("README.md");
    if readme_path.exists() && is_path_allowed(&readme_path, true, workspace_root) {
        if let Ok(readme_content) = fs::read_to_string(&readme_path) {
            content = Some(render_markdown(&readme_content, workspace_root));
            source_file = Some("README.md".to_string());
        }
    }

    let about_path = project_path.join("ABOUT");
    if about_path.exists() {
        if let Some((about_tags, about_sent)) = parse_about_file(&about_path) {
            if content.is_none() {
                content = about_sent
                    .clone()
                    .map(|s| render_markdown(&s, workspace_root));
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
            zip.start_file(name.to_string_lossy(), options.clone()).ok()?;
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
        highlighted_dark_code: None,
        highlighted_light_code: None,
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

    return Ok(HttpResponse::Ok().content_type("text/html").body(body));
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
            let filename = format!("{}.zip", canonical_path.file_name().unwrap().to_string_lossy());
            return Ok(HttpResponse::Ok()
                .content_type("application/zip")
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

    let file_content = fs::read(&canonical_path)
        .map_err(|_| actix_web::error::ErrorNotFound("File not found"))?;

    Ok(HttpResponse::Ok()
        .content_type(content_type)
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
            highlighted_dark_code: None,
            highlighted_light_code: None,
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
            return Err(actix_web::error::ErrorNotFound("Path not found"))
        },
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
        canonical_path.parent().unwrap_or(&canonical_path).to_path_buf()
    };

    let dir_contents = get_directory_contents(&current_dir, true, workspace_root);
    let parent_dir = if current_dir != Path::new(workspace_root) {
        current_dir
            .parent()
            .and_then(|p| p.strip_prefix(workspace_root).ok())
            .map(|p| p.to_string_lossy().into_owned())
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
        highlighted_dark_code: None,
        highlighted_light_code: None,
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

        let highlighted_code =
            highlight_code(&canonical_path, &content, &data.syntax_set, &data.theme_set);

        context.highlighted_dark_code = Some(highlighted_code.dark);
        context.highlighted_light_code = Some(highlighted_code.light);
        context.lines_count = Some(content.lines().count());
        context.file_size = Some(file_info.size);
        context.last_modified = Some(file_info.last_modified);

        let body = data
            .tera
            .render("code_view.html", &context.into_context())
            .map_err(|_| actix_web::error::ErrorInternalServerError("Internal server error"))?;

        return Ok(HttpResponse::Ok()
            .content_type("text/html; charset=utf-8")
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
            .insert_header(("X-Content-Type-Options", "nosniff"))
            .insert_header(("X-Frame-Options", "DENY"))
            .insert_header(("X-XSS-Protection", "1; mode=block"))
            .body(body));
    }

    let (content, tags, source_file, about_sentence) =
        get_project_content(&canonical_path, &workspace_root);
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
    context.tags = tags.into_iter().map(|t| encode_text(&t).to_string()).collect();

    let body = data
        .tera
        .render("repo_view.html", &context.into_context())
        .map_err(|_| actix_web::error::ErrorInternalServerError("Internal server error"))?;

    Ok(HttpResponse::Ok()
        .content_type("text/html; charset=utf-8")
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
                    .add(("Strict-Transport-Security", "max-age=31536000; includeSubDomains".to_string()))
                    .add((
                        "Content-Security-Policy",
                        "default-src 'self'; \
                         script-src 'self' 'unsafe-inline' 'unsafe-eval'; \
                         style-src 'self' 'unsafe-inline' https://cdnjs.cloudflare.com; \
                         font-src 'self' https://cdnjs.cloudflare.com; \
                         img-src 'self' data: https:; \
                         connect-src 'self';".to_string()
                    ))
                    .add(("Referrer-Policy", "strict-origin-when-cross-origin".to_string()))
            )
            .service(index)
            .service(download_file)
            .service(view_path)
    })
    .bind(("127.0.0.1", 8201))?
    .workers(16)
    .run()
    .await
}
