"""Pure Confluence storage-format helpers shared by Gateway capability brokers."""

from __future__ import annotations

import html
import re

_INLINE_CODE = re.compile(r"`([^`\n]+)`")
_LINK = re.compile(r"\[([^\]]+)\]\(([^)\s]+)(?:\s+\"[^\"]*\")?\)")
_BOLD = re.compile(r"\*\*([^*\n]+)\*\*")
_ITALIC = re.compile(r"(?<!\*)\*([^*\n]+)\*(?!\*)")
_FENCED_CODE = re.compile(r"```([a-zA-Z0-9_+-]*)\n(.*?)```", re.DOTALL)
_LINE_MARKER = re.compile(
    r"(?m)(^\s{0,3}#{1,6}\s+\S)|(^\s{0,3}[-*]\s+\S)|(^\s{0,3}\d+\.\s+\S)|(^```)"
)
_INLINE_MARKER = re.compile(
    r"\*\*[^*\n]+\*\*|(?<!\*)\*[^*\n]+\*(?!\*)|`[^`\n]+`|\[[^\]]+\]\([^)\s]+\)"
)


def escape_storage_text(value: str) -> str:
    """Escape plain text for insertion into existing storage XHTML."""

    return html.escape(value, quote=False)


def _inline(value: str) -> str:
    code: list[str] = []

    def stash(match: re.Match[str]) -> str:
        code.append(f"<code>{escape_storage_text(match.group(1))}</code>")
        return f"\x00CODE{len(code) - 1}\x00"

    rendered = _INLINE_CODE.sub(stash, value)
    rendered = escape_storage_text(rendered)
    for index, segment in enumerate(code):
        rendered = rendered.replace(f"\x00CODE{index}\x00", segment)
    def link(match: re.Match[str]) -> str:
        target = match.group(2)
        if not (target.startswith(("https://", "http://", "/"))):
            return match.group(1)
        return f'<a href="{html.escape(target, quote=True)}">{match.group(1)}</a>'

    rendered = _LINK.sub(link, rendered)
    rendered = _BOLD.sub(r"<strong>\1</strong>", rendered)
    return _ITALIC.sub(r"<em>\1</em>", rendered)


def _looks_like_markdown(value: str) -> bool:
    if _LINE_MARKER.search(value) or _INLINE_MARKER.search(value):
        return True
    untagged = re.sub(r"<[^>]+>", "\n", value)
    return bool(_LINE_MARKER.search(untagged) or _INLINE_MARKER.search(untagged))


def markdown_to_storage(value: str) -> str:
    """Convert bounded Markdown-ish content to Confluence storage XHTML.

    Raw XHTML is preserved only when it contains no Markdown markers. This
    prevents the common ``<p># heading</p>`` input from rendering literal
    Markdown while retaining advanced Confluence macros.
    """

    if not value:
        return "<p></p>"
    content = value.strip()
    if content.startswith("<") and content.endswith(">") and not _looks_like_markdown(content):
        return value
    if _looks_like_markdown(content) and re.search(
        r"</?(?:p|div|br|span)\b", content, flags=re.IGNORECASE
    ):
        content = re.sub(
            r"</?(?:p|div|br\s*/?|span)(?:\s[^>]*)?>",
            "\n",
            content,
            flags=re.IGNORECASE,
        )
        content = re.sub(r"\n{3,}", "\n\n", content).strip()

    code_blocks: list[tuple[str, str]] = []

    def stash_fenced(match: re.Match[str]) -> str:
        code_blocks.append((match.group(1) or "", match.group(2)))
        return f"\x00FENCED{len(code_blocks) - 1}\x00"

    content = _FENCED_CODE.sub(stash_fenced, content)
    output: list[str] = []
    for block in re.split(r"\n\s*\n", content):
        block = block.rstrip()
        if not block.strip():
            continue
        lines = block.split("\n")
        if len(lines) == 1 and re.fullmatch(r"\x00FENCED\d+\x00", lines[0]):
            index = int(lines[0][7:-1])
            language, body = code_blocks[index]
            body = body.replace("]]>", "]]]]><![CDATA[>")
            language_tag = (
                f'<ac:parameter ac:name="language">{escape_storage_text(language)}</ac:parameter>'
                if language
                else ""
            )
            output.append(
                '<ac:structured-macro ac:name="code">'
                f"{language_tag}<ac:plain-text-body><![CDATA[{body}]]></ac:plain-text-body>"
                "</ac:structured-macro>"
            )
            continue
        if len(lines) == 1 and (heading := re.match(r"^(#{1,6})\s+(.*)$", lines[0])):
            level = len(heading.group(1))
            output.append(f"<h{level}>{_inline(heading.group(2))}</h{level}>")
            continue
        if all(re.match(r"^\s*[-*]\s+", line) for line in lines):
            items = []
            for line in lines:
                item = re.sub(r"^\s*[-*]\s+", "", line)
                items.append(f"<li>{_inline(item)}</li>")
            output.append("<ul>" + "".join(items) + "</ul>")
            continue
        if all(re.match(r"^\s*\d+\.\s+", line) for line in lines):
            items = []
            for line in lines:
                item = re.sub(r"^\s*\d+\.\s+", "", line)
                items.append(f"<li>{_inline(item)}</li>")
            output.append("<ol>" + "".join(items) + "</ol>")
            continue
        output.append("<p>" + "<br/>".join(_inline(line) for line in lines) + "</p>")
    rendered = "".join(output) or "<p></p>"
    return re.sub(r"</ol>\s*<ol>", "", re.sub(r"</ul>\s*<ul>", "", rendered))
