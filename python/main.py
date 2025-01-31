import os
from datetime import datetime
import zipfile
import io
import mimetypes
import re
from functools import cache

import pathspec
import humanize
import markdown2
import bleach
from flask import Flask, render_template, abort, send_file, url_for
from pygments import highlight
from pygments.lexers import get_lexer_for_filename, guess_lexer
from pygments.formatters.html import HtmlFormatter

app = Flask(__name__)

WORKSPACE_ROOT = os.path.join(os.path.abspath(os.getcwd()), 'Projects')

ALLOWED_TAGS = [
    'h1', 'h2', 'h3', 'h4', 'h5', 'h6', 'p', 'div', 'span', 'br', 'hr',
    'ul', 'ol', 'li', 'dl', 'dt', 'dd',
    'strong', 'em', 'i', 'b', 'code', 'pre',
    'a', 'img', 'table', 'thead', 'tbody', 'tr', 'th', 'td',
    'blockquote', 'sup', 'sub', 'strike'
]

ALLOWED_ATTRIBUTES = {
    '*': ['class', 'id', 'title'],
    'a': ['href', 'target', 'rel'],
    'img': ['src', 'alt', 'title', 'width', 'height'],
    'td': ['align', 'valign'],
    'th': ['align', 'valign', 'scope'],
    'code': ['class']
}

def get_gitignore_spec(project_path):
    """Get gitignore spec for a project directory."""
    gitignore_path = os.path.join(project_path, '.gitignore')
    if not os.path.exists(gitignore_path):
        return None

    try:
        with open(gitignore_path, 'r', encoding='utf-8') as f:
            gitignore = f.read()

        return pathspec.PathSpec.from_lines('gitwildmatch', gitignore.splitlines())
    except Exception as e:
        print(f"Error reading .gitignore: {e}")
        return None

def is_path_matches_gitignore(path, gitignore_rules):
    """Check if path matches any gitignore rules."""
    if not gitignore_rules:
        return False

    return gitignore_rules.match_file(path)

def is_path_allowed(path, check_gitignore=True, allow_about=False):
    """Check if a path is allowed based on .gitignore rules.
    
    Args:
        path: The path to check
        check_gitignore: Whether to check gitignore rules (default: True)
        allow_about: Whether to allow access to ABOUT file (default: False)
    """
    if not os.path.exists(path):
        return False

    if path == WORKSPACE_ROOT:
        return True

    rel_path = os.path.relpath(path, WORKSPACE_ROOT)
    if rel_path == '.' or not rel_path:
        return False

    project_root = os.path.join(WORKSPACE_ROOT, rel_path.split(os.sep)[0])
    if not os.path.isdir(project_root):
        return False

    if '.git' in rel_path.split(os.sep):
        return False

    if os.path.basename(path) == 'ABOUT':
        return allow_about

    if path == project_root:
        return True

    if check_gitignore:
        project_rel_path = os.path.relpath(path, project_root)

        gitignore_rules = get_gitignore_spec(project_root)
        if is_path_matches_gitignore(project_rel_path, gitignore_rules):
            return False

    return True

def secure_join_paths(*paths):
    """Securely join paths and validate the result is within workspace root."""
    path = os.path.normpath(os.path.join(*paths))
    if not path.startswith(WORKSPACE_ROOT):
        return None
    return path

def get_relative_path(abs_path):
    """Convert absolute path to path relative to workspace root."""
    try:
        rel_path = os.path.relpath(abs_path, WORKSPACE_ROOT)
        return rel_path if rel_path != '.' else ''
    except ValueError:
        return ''

def get_file_info(file_path, check_gitignore=True):
    """Get file information including size and line count."""

    allow_about = os.path.basename(file_path) == 'ABOUT' and \
        os.path.dirname(file_path) == os.path.dirname(os.path.dirname(file_path))
    if not is_path_allowed(file_path, check_gitignore=check_gitignore, allow_about=allow_about):
        return None

    try:
        stats = os.stat(file_path)
        size = stats.st_size
        if os.path.isfile(file_path):
            with open(file_path, 'r', encoding='utf-8', errors='ignore') as f:
                lines_count = sum(1 for _ in f)
        else:
            lines_count = None

        return {
            'file_size': humanize.naturalsize(size),
            'lines_count': lines_count,
            'last_modified': datetime.fromtimestamp(stats.st_mtime).strftime('%b %d, %Y %H:%M')
        }
    except Exception as e:
        print(f"Error getting file info for {file_path}: {e}")
        return None

def create_zip_file(directory_path):
    """Create a ZIP file from a directory."""
    if not is_path_allowed(directory_path):
        return None

    memory_file = io.BytesIO()

    try:
        with zipfile.ZipFile(memory_file, 'w', zipfile.ZIP_DEFLATED) as zf:
            for root, _, files in os.walk(directory_path):
                for file in files:
                    file_path = os.path.join(root, file)
                    if is_path_allowed(file_path):
                        arc_name = os.path.relpath(file_path, directory_path)
                        zf.write(file_path, arc_name)

        memory_file.seek(0)
        return memory_file
    except Exception as e:
        print(f"Error creating zip for {directory_path}: {e}")
        return None

def get_directory_contents(path, check_gitignore=True):
    """Get sorted directory contents with file information.
    
    Args:
        path: The directory path to list
        check_gitignore: Whether to check gitignore rules (default: True)
    """

    if path == WORKSPACE_ROOT:
        contents = []
        try:
            items = os.listdir(path)
            for item in sorted(
                items, key =
                    lambda x: (
                        not os.path.isdir(os.path.join(path, x)),
                        x.lower()
                    )
                ):

                full_path = os.path.join(path, item)
                if not os.path.isdir(full_path):
                    continue

                info = get_file_info(full_path, check_gitignore=False)
                if not info:
                    continue

                contents.append({
                    'name': item,
                    'path': get_relative_path(full_path),
                    'is_dir': True,
                    'size': info['file_size'],
                    'last_modified': info['last_modified']
                })
            return contents
        except Exception as e:
            print(f"Error reading workspace root: {e}")
            return []

    if not is_path_allowed(path, check_gitignore=check_gitignore):
        return []

    contents = []
    try:
        items = os.listdir(path)
        for item in sorted(
            items, key =
                lambda x: (
                    not os.path.isdir(os.path.join(path, x)),
                    x.lower()
                )
            ):

            full_path = os.path.join(path, item)
            if not is_path_allowed(full_path, check_gitignore=check_gitignore):
                continue

            info = get_file_info(full_path, check_gitignore=check_gitignore)
            if not info:
                continue

            contents.append({
                'name': item,
                'path': get_relative_path(full_path),
                'is_dir': os.path.isdir(full_path),
                'size': info['file_size'],
                'last_modified': info['last_modified']
            })
        return contents
    except Exception as e:
        print(f"Error reading directory {path}: {e}")
        return []

@cache
def highlight_code(file_path, code_content):
    """Highlight code using Pygments with line numbers."""
    try:
        lexer = get_lexer_for_filename(file_path)
    except Exception:
        lexer = guess_lexer(code_content)

    formatter = HtmlFormatter(style='monokai', linenos=True)
    return highlight(code_content, lexer, formatter)

def is_binary_file(file_path):
    """Check if a file is binary."""
    try:
        with open(file_path, 'rb') as f:
            f.read(1024)
            return False
    except:
        return True

def parse_about_file(project_path):
    """Parse the ABOUT file in a project directory."""
    about_path = os.path.join(project_path, 'ABOUT')
    if not os.path.exists(about_path):
        return None, [], None

    try:
        with open(about_path, 'r', encoding='utf-8') as f:
            content = f.read().strip()

        lines = content.split('\n')

        tags = [line.strip('#').strip() for line in lines if line.strip().startswith('#')]

        content_lines = [line for line in lines if not line.strip().startswith('#')]
        about_sentence = next((line for line in content_lines if line.strip()), None)

        return tags, about_sentence
    except Exception as e:
        print(f"Error reading ABOUT file: {e}")
        return [], None

def is_project_root(path):
    """Check if the given path is a project root directory."""
    # Check if this is a direct subdirectory of the workspace root (Projects directory)
    return os.path.dirname(path) == WORKSPACE_ROOT and os.path.isdir(path)

@cache
def render_markdown(content, base_path):
    """Render Markdown content with security measures and proper image/link handling."""
    markdown = markdown2.Markdown(extras=[
        'fenced-code-blocks',
        'tables',
        'header-ids',
        'task-lists',
        'code-friendly',
        'smarty-pants',
        'metadata',
        'strike',
        'target-blank-links'
    ])

    html = markdown.convert(content)

    def fix_relative_urls(match):
        attr = match.group(1)  # src or href
        url = match.group(2)
        
        # Don't modify absolute URLs or anchors
        if url.startswith(('http://', 'https://', '/', '#', 'mailto:')):
            return f'{attr}="{url}"'
            
        # Handle relative URLs
        if url.lower().endswith(('.png', '.jpg', '.jpeg', '.gif', '.svg')):
            return f'{attr}="{url_for("view_path", file_path=os.path.join(base_path, url))}"'
        return f'{attr}="{url_for("view_path", file_path=os.path.join(base_path, url))}"'

    html = re.sub(r'(src|href)=["\']([^"\']+)["\']', fix_relative_urls, html)

    clean_html = bleach.clean(
        html,
        tags=ALLOWED_TAGS,
        attributes=ALLOWED_ATTRIBUTES,
        protocols=['http', 'https', 'data', 'mailto'],
        strip=True
    )

    return clean_html

def get_project_content(project_path):
    """Get project content from README.md or ABOUT file."""
    readme_path = os.path.join(project_path, 'README.md')
    about_path = os.path.join(project_path, 'ABOUT')

    content = None
    tags = []
    source_file = None
    about_sentence = None

    if os.path.exists(readme_path) and is_path_allowed(readme_path, check_gitignore=True):
        try:
            with open(readme_path, 'r', encoding='utf-8') as f:
                content = f.read()
            source_file = 'README.md'
        except Exception as e:
            print(f"Error reading README.md: {e}")

    if os.path.exists(about_path):
        try:
            if is_path_allowed(about_path, check_gitignore=True, allow_about=True):
                about_tags, about_sent = parse_about_file(project_path)
                tags = about_tags or []
                about_sentence = about_sent
        except Exception as e:
            print(f"Error reading ABOUT file: {e}")

    if content:
        relative_path = get_relative_path(project_path)
        content = render_markdown(content, relative_path)

    return content, tags, source_file, about_sentence

@app.route('/')
def index():
    """
    Render the index page.
    """
    contents = get_directory_contents(WORKSPACE_ROOT, check_gitignore=False)
    return render_template('index.html', contents=contents)

@app.route('/download/')
@app.route('/download/<path:file_path>')
def download_file(file_path = ""):
    """
    Download a file or directory at the given path.
    """
    try:
        abs_path = secure_join_paths(WORKSPACE_ROOT, file_path)
        if not abs_path or not is_path_allowed(abs_path):
            abort(404)

        if not os.path.isdir(abs_path):
            mime_type, _ = mimetypes.guess_type(abs_path)
            return send_file(
                abs_path,
                mimetype=mime_type or 'application/octet-stream',
                as_attachment=True,
                download_name=os.path.basename(file_path)
            )

        memory_file = create_zip_file(abs_path)
        if not memory_file:
            abort(404)

        return send_file(
            memory_file,
            mimetype='application/zip',
            as_attachment=True,
            download_name=f"{os.path.basename(file_path)}.zip"
        )
    except Exception as e:
        print(f"Error in download_file: {e}")
        abort(404)

@app.route('/view/')
@app.route('/view/<path:file_path>')
def view_path(file_path = ""):
    """
    View a file or directory at the given path.

    Args:
        file_path (str): Relative path to the file or directory to view. Defaults to empty string.

    Returns:
        Response: Rendered template showing either file contents or directory listing.
        If file_path is empty, returns the index page.
        If path is invalid or inaccessible, returns 404.
        If file is binary, returns 400 error.
    """

    try:
        if not file_path:
            return index()

        abs_path = secure_join_paths(WORKSPACE_ROOT, file_path)
        if not abs_path or not is_path_allowed(abs_path):
            abort(404)

        current_dir = os.path.dirname(abs_path) if not os.path.isdir(abs_path) else abs_path
        if not is_path_allowed(current_dir):
            abort(404)

        dir_contents = get_directory_contents(current_dir)
        parent_dir = get_relative_path(
            os.path.dirname(current_dir)
        ) if current_dir != WORKSPACE_ROOT else None

        if not os.path.isdir(abs_path):
            if is_binary_file(abs_path):
                return "Binary file not displayed", 400

            with open(abs_path, 'r', encoding='utf-8', errors='ignore') as f:
                content = f.read()

            file_info = get_file_info(abs_path)
            if not file_info:
                abort(404)

            highlighted_code = highlight_code(abs_path, content)

            return render_template(
                'code_view.html',
                file_path=file_path,
                is_dir=False,
                dir_contents=dir_contents,
                highlighted_code=highlighted_code,
                parent_dir=parent_dir,
                workspace_root=WORKSPACE_ROOT,
                **file_info
            )

        contents = get_directory_contents(abs_path)
        project_name = os.path.basename(abs_path)

        if not is_project_root(abs_path):
            return render_template(
                'code_view.html',
                file_path=file_path,
                is_dir=True,
                contents=contents,
                dir_contents=dir_contents,
                parent_dir=parent_dir,
                workspace_root=WORKSPACE_ROOT
            )

        content, tags, source_file, about_sentence = get_project_content(abs_path)

        return render_template(
            'repo_view.html',
            file_path=file_path,
            project_name=project_name,
            about_content=content or "No description provided.",
            content_source=source_file,
            about_sentence=about_sentence,
            tags=tags,
            contents=contents,
            dir_contents=dir_contents,
            parent_dir=parent_dir,
            workspace_root=WORKSPACE_ROOT
        )

    except Exception as e:
        print(f"Error in view_path: {e}")
        abort(404)

if __name__ == '__main__':
    app.run()
