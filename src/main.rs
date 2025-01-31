use std::env;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;

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

const DEFAULT_WORKSPACE_ROOT: &str = "/etc/tn3wrepo/Projects";

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

fn is_path_allowed(path: &Path, check_gitignore: bool, workspace_root: &str) -> bool {
    if !path.exists() {
        return false;
    }

    if path == Path::new(workspace_root) {
        return true;
    }

    let rel_path = match path.strip_prefix(workspace_root) {
        Ok(p) => p,
        Err(_) => return false,
    };

    let project_root = PathBuf::from(workspace_root).join(
        rel_path
            .components()
            .next()
            .unwrap_or_else(|| std::path::Component::Normal("".as_ref())),
    );

    if !project_root.is_dir() {
        return false;
    }

    if rel_path.components().any(|c| c.as_os_str() == ".git") {
        return false;
    }

    if path.file_name().map_or(false, |n| n == "ABOUT") {
        let backtrace = std::backtrace::Backtrace::capture();
        let is_from_get_project_content = backtrace.to_string().contains("get_project_content");
        return is_from_get_project_content;
    }

    if path == project_root {
        return true;
    }

    if check_gitignore {
        if let Some(gitignore) = get_gitignore(&project_root) {
            let rel_to_project = path.strip_prefix(&project_root).unwrap_or(rel_path);
            if gitignore.matched(rel_to_project, false).is_ignore() {
                return false;
            }
        }
    }

    true
}

fn get_file_info(path: &Path, workspace_root: &str) -> Option<FileInfo> {
    let metadata = path.metadata().ok()?;
    let name = path.file_name()?.to_string_lossy().into_owned();
    let rel_path = path.strip_prefix(workspace_root).ok()?;

    Some(FileInfo {
        name,
        path: rel_path.to_string_lossy().into_owned(),
        is_dir: metadata.is_dir(),
        size: format_size(metadata.len(), BINARY),
        last_modified: DateTime::<Local>::from(metadata.modified().ok()?)
            .format("%b %d, %Y %H:%M")
            .to_string(),
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
        let html = html.replace(r#"<pre style="background-color:#2b303b;">"#, "<pre>");

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
        .unwrap_or_else(|_| content.to_string());

    let dark_html = highlighted_html_for_string(content, ss, syntax, dark_theme)
        .map(|html| process_html(html))
        .unwrap_or_else(|_| content.to_string());

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
    let mut html_output = String::new();
    html::push_html(&mut html_output, parser);

    let html_with_fixed_urls = html_output
        .replace("src=\"./", &format!("src=\"/{}/", base_path))
        .replace("href=\"./", &format!("href=\"/{}/", base_path));

    html_with_fixed_urls
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
    path.parent() == Some(Path::new(workspace_root))
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

    if !is_path_allowed(&file_path, true, workspace_root) {
        return Err(actix_web::error::ErrorNotFound("File not found"));
    }

    if file_path.is_dir() {
        if let Some(zip_data) = create_zip_file(&file_path, &workspace_root) {
            let filename = format!("{}.zip", file_path.file_name().unwrap().to_string_lossy());
            return Ok(HttpResponse::Ok()
                .content_type("application/zip")
                .append_header((
                    "Content-Disposition",
                    format!("attachment; filename=\"{}\"", filename),
                ))
                .body(zip_data));
        }
        return Err(actix_web::error::ErrorInternalServerError(
            "Failed to create zip",
        ));
    }

    let filename = file_path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("download");

    let content_type = match file_path.extension().and_then(|ext| ext.to_str()) {
        Some("txt") => "text/plain",
        Some("html") | Some("htm") => "text/html",
        Some("css") => "text/css",
        Some("js") => "application/javascript",
        Some("json") => "application/json",
        Some("png") => "image/png",
        Some("jpg") | Some("jpeg") => "image/jpeg",
        Some("gif") => "image/gif",
        Some("pdf") => "application/pdf",
        Some("zip") => "application/zip",
        Some("md") => "text/markdown",
        Some("rs") => "text/rust",
        Some("py") => "text/python",
        Some("go") => "text/golang",
        Some("java") => "text/java",
        Some("c") | Some("cpp") | Some("h") | Some("hpp") => "text/c",
        _ => "application/octet-stream",
    };

    let file_content =
        fs::read(&file_path).map_err(|_| actix_web::error::ErrorNotFound("File not found"))?;

    Ok(HttpResponse::Ok()
        .content_type(content_type)
        .append_header((
            "Content-Disposition",
            format!("attachment; filename=\"{}\"", filename),
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
            .map_err(|e| {
                eprintln!("Template error: {}", e);
                actix_web::error::ErrorInternalServerError("Template error")
            })?;

        return Ok(HttpResponse::Ok().content_type("text/html").body(body));
    }

    let file_path = PathBuf::from(workspace_root).join(&path_str);

    if !is_path_allowed(&file_path, true, workspace_root) {
        return Err(actix_web::error::ErrorNotFound("Path not found"));
    }

    let current_dir = if file_path.is_dir() {
        file_path.clone()
    } else {
        file_path.parent().unwrap_or(&file_path).to_path_buf()
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
        is_dir: file_path.is_dir(),
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

    if !file_path.is_dir() {
        if is_binary_file(&file_path) {
            return Err(actix_web::error::ErrorBadRequest("Binary file"));
        }

        let content = fs::read_to_string(&file_path)
            .map_err(|_| actix_web::error::ErrorNotFound("File not found"))?;

        let file_info = get_file_info(&file_path, workspace_root)
            .ok_or_else(|| actix_web::error::ErrorNotFound("File not found"))?;

        let highlighted_code =
            highlight_code(&file_path, &content, &data.syntax_set, &data.theme_set);

        context.highlighted_dark_code = Some(highlighted_code.dark);
        context.highlighted_light_code = Some(highlighted_code.light);
        context.lines_count = Some(content.lines().count());
        context.file_size = Some(file_info.size);
        context.last_modified = Some(file_info.last_modified);

        let body = data
            .tera
            .render("code_view.html", &context.into_context())
            .map_err(|e| {
                eprintln!("Template error: {}", e);
                actix_web::error::ErrorInternalServerError("Template error")
            })?;

        return Ok(HttpResponse::Ok().content_type("text/html").body(body));
    }

    context.contents = get_directory_contents(&file_path, true, workspace_root);

    if !is_project_root(&file_path, workspace_root) {
        let body = data
            .tera
            .render("code_view.html", &context.into_context())
            .map_err(|e| {
                eprintln!("Template error: {}", e);
                actix_web::error::ErrorInternalServerError("Template error")
            })?;

        return Ok(HttpResponse::Ok().content_type("text/html").body(body));
    }

    let (content, tags, source_file, about_sentence) =
        get_project_content(&file_path, &workspace_root);
    context.project_name = Some(
        file_path
            .file_name()
            .unwrap()
            .to_string_lossy()
            .into_owned(),
    );
    context.about_content = content;
    context.content_source = source_file;
    context.about_sentence = about_sentence;
    context.tags = tags;

    let body = data
        .tera
        .render("repo_view.html", &context.into_context())
        .map_err(|e| {
            eprintln!("Template error: {}", e);
            actix_web::error::ErrorInternalServerError("Template error")
        })?;

    Ok(HttpResponse::Ok().content_type("text/html").body(body))
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
            .service(index)
            .service(download_file)
            .service(view_path)
    })
    .bind(("127.0.0.1", 8201))?
    .workers(16)
    .run()
    .await
}
