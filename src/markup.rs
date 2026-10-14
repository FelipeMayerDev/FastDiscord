//! Discord-markup tokenizer: turns message content into styled segments.
//! Pure and unit-tested; the UI layer maps segments to `egui::TextFormat`
//! and lays quotes and code blocks out as blocks.
//!
//! Blocks: ``` fences, `>`/`>>>` quotes, `#`/`##`/`###` headings, `-#`
//! subtext and `-`/`*`/`1.` lists. Inline, nestable: `**bold**`,
//! `*italic*`/`_italic_`, `__underline__`, `~~strike~~`, `||spoiler||`,
//! plus `code`, mentions, custom emoji, bare/`<…>`/`[masked](…)` links and
//! `\` escapes.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Style {
    Normal,
    Code,
    CodeBlock,
    /// `<@id>` / `<@!id>` — rendered as `@id`, resolved by the UI if known.
    Mention,
    /// `<#id>` — rendered as `#id`, resolved by the UI if known.
    ChannelName,
    /// `<:name:id>` / `<a:name:id>`, kept whole: the UI shows the image,
    /// `:name:` as its fallback.
    Emoji,
    Link,
}

/// Inline emphasis, combinable (`***__x__***` is all three).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Marks {
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    pub strike: bool,
    pub spoiler: bool,
}

/// A line's text size: headings, `-#` subtext or regular.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Size {
    #[default]
    Normal,
    H1,
    H2,
    H3,
    Small,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Segment {
    pub text: String,
    pub style: Style,
    pub marks: Marks,
    pub size: Size,
    /// Inside a `>` / `>>>` block quote.
    pub quote: bool,
    /// A masked link's target; bare links are their own text.
    pub url: Option<String>,
}

/// What applies to the text being parsed: the line's block state plus the
/// emphasis opened around it.
#[derive(Clone, Copy, Default)]
struct Ctx {
    marks: Marks,
    size: Size,
    quote: bool,
}

impl Ctx {
    fn segment(self, text: impl Into<String>, style: Style) -> Segment {
        Segment {
            text: text.into(),
            style,
            marks: self.marks,
            size: self.size,
            quote: self.quote,
            url: None,
        }
    }
}

pub fn tokenize(text: &str) -> Vec<Segment> {
    let mut out = Vec::new();
    // `>>>` quotes everything after it, across lines.
    let mut quote_rest = false;
    let mut rest = text;
    while let Some(start) = rest.find("```") {
        prose(&rest[..start], &mut quote_rest, &mut out);
        let after = &rest[start + 3..];
        let (mut body, next) = match after.find("```") {
            Some(end) => (&after[..end], &after[end + 3..]),
            None => (after, ""),
        };
        // Drop a first line that is only a language tag.
        if let Some(nl) = body.find('\n')
            && body[..nl]
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '+')
        {
            body = &body[nl + 1..];
        }
        let body = body.trim_end_matches('\n');
        if !body.is_empty() {
            out.push(Ctx::default().segment(body, Style::CodeBlock));
        }
        rest = next;
    }
    prose(rest, &mut quote_rest, &mut out);
    trim_block_edges(&mut out);
    out
}

/// Text between code fences, line by line: block prefixes first, then the
/// inline markup.
// ponytail: emphasis doesn't span lines (`**a\nb**` stays plain); parse
// whole paragraphs if multi-line bold/spoilers turn up.
fn prose(text: &str, quote_rest: &mut bool, out: &mut Vec<Segment>) {
    for (index, line) in text.split('\n').enumerate() {
        if index > 0 {
            let quote = out.last().is_some_and(|segment: &Segment| segment.quote);
            out.push(
                Ctx {
                    quote,
                    ..Ctx::default()
                }
                .segment("\n", Style::Normal),
            );
        }
        line_markup(line, quote_rest, out);
    }
}

fn line_markup(mut line: &str, quote_rest: &mut bool, out: &mut Vec<Segment>) {
    let mut ctx = Ctx::default();
    if *quote_rest {
        ctx.quote = true;
    } else if let Some(rest) = line.strip_prefix(">>> ") {
        *quote_rest = true;
        ctx.quote = true;
        line = rest;
    } else if let Some(rest) = line.strip_prefix("> ") {
        ctx.quote = true;
        line = rest;
    }
    for (prefix, size) in [
        ("### ", Size::H3),
        ("## ", Size::H2),
        ("# ", Size::H1),
        ("-# ", Size::Small),
    ] {
        if let Some(rest) = line.strip_prefix(prefix) {
            ctx.size = size;
            line = rest;
            break;
        }
    }
    if ctx.size == Size::Normal {
        let body = line.trim_start_matches(' ');
        let depth = (line.len() - body.len()) / 2;
        let ordinal = body
            .find(". ")
            .filter(|&dot| dot > 0 && body[..dot].bytes().all(|b| b.is_ascii_digit()));
        let marker = if let Some(rest) = body.strip_prefix("- ").or(body.strip_prefix("* ")) {
            line = rest;
            Some(if depth == 0 { "•" } else { "◦" }.to_string())
        } else if let Some(dot) = ordinal {
            line = &body[dot + 2..];
            Some(format!("{}.", &body[..dot]))
        } else {
            None
        };
        if let Some(marker) = marker {
            let indent = "    ".repeat(depth);
            out.push(ctx.segment(format!("{indent} {marker} "), Style::Normal));
        }
    }
    inline(line, ctx, out);
}

/// The end of a `marker`-wrapped span's content in `body` (the text after
/// the opener): a non-empty match not followed by another marker char, so
/// `***x***` closes its `**` after the italic `*`.
fn closing(body: &str, marker: &str) -> Option<usize> {
    let lead = marker.as_bytes()[0];
    let mut from = 0;
    while let Some(found) = body[from..].find(marker) {
        let at = from + found;
        if at > 0 && body.as_bytes().get(at + marker.len()) != Some(&lead) {
            return Some(at);
        }
        from = at + 1;
    }
    None
}

/// A single `*`/`_` italic's end: skips doubled markers (inner bold or
/// underline) and needs text right before it.
fn italic_end(body: &str, marker: u8) -> Option<usize> {
    let bytes = body.as_bytes();
    let mut at = 0;
    while at < bytes.len() {
        if bytes[at] == marker {
            if bytes.get(at + 1) == Some(&marker) {
                at += 2;
                continue;
            }
            if at > 0 && !bytes[at - 1].is_ascii_whitespace() {
                return Some(at);
            }
        }
        at += 1;
    }
    None
}

/// A closing `>` for an embed-like token, only if it comes soon enough that
/// the token can plausibly be one (guards against swallowing whole messages
/// after a stray `<`).
fn plausible_close(rest: &str) -> Option<usize> {
    rest.find('>').filter(|&idx| idx <= 96)
}

fn is_web_url(text: &str) -> bool {
    text.starts_with("https://") || text.starts_with("http://")
}

fn inline(text: &str, ctx: Ctx, out: &mut Vec<Segment>) {
    let mut plain = String::new();
    let flush = |plain: &mut String, out: &mut Vec<Segment>| {
        if !plain.is_empty() {
            out.push(ctx.segment(std::mem::take(plain), Style::Normal));
        }
    };
    let mut i = 0usize;
    while i < text.len() {
        let rest = &text[i..];

        if let Some(escaped) = rest
            .strip_prefix('\\')
            .and_then(|after| after.chars().next())
            .filter(char::is_ascii_punctuation)
        {
            plain.push(escaped);
            i += 2;
            continue;
        }

        if rest.starts_with('`') {
            let ticks = if rest.starts_with("``") { 2 } else { 1 };
            let marker = &rest[..ticks];
            match rest[ticks..].find(marker) {
                Some(end) if end > 0 => {
                    flush(&mut plain, out);
                    out.push(ctx.segment(&rest[ticks..ticks + end], Style::Code));
                    i += ticks + end + ticks;
                }
                _ => {
                    plain.push_str(marker);
                    i += ticks;
                }
            }
            continue;
        }

        let paired: [(&str, fn(&mut Marks)); 4] = [
            ("||", |marks| marks.spoiler = true),
            ("**", |marks| marks.bold = true),
            ("__", |marks| marks.underline = true),
            ("~~", |marks| marks.strike = true),
        ];
        if let Some((marker, apply)) = paired.iter().find(|(marker, _)| rest.starts_with(marker)) {
            let body = &rest[2..];
            if let Some(end) = closing(body, marker) {
                flush(&mut plain, out);
                let mut inner = ctx;
                apply(&mut inner.marks);
                inline(&body[..end], inner, out);
                i += 2 + end + 2;
            } else {
                plain.push_str(marker);
                i += 2;
            }
            continue;
        }

        if rest.starts_with('*') || rest.starts_with('_') {
            let marker = rest.as_bytes()[0];
            let body = &rest[1..];
            // `_` needs word edges, so snake_case stays plain.
            let word_before = text[..i]
                .chars()
                .next_back()
                .is_some_and(char::is_alphanumeric);
            let end = italic_end(body, marker).filter(|&end| {
                !body.starts_with(char::is_whitespace)
                    && (marker == b'*'
                        || !word_before && !body[end + 1..].starts_with(char::is_alphanumeric))
            });
            match end {
                Some(end) => {
                    flush(&mut plain, out);
                    let mut inner = ctx;
                    inner.marks.italic = true;
                    inline(&body[..end], inner, out);
                    i += 1 + end + 1;
                }
                None => {
                    plain.push(marker as char);
                    i += 1;
                }
            }
            continue;
        }

        if rest.starts_with("<@") {
            if let Some(close) = plausible_close(rest) {
                flush(&mut plain, out);
                let id = rest[2..close].trim_start_matches('!');
                out.push(ctx.segment(format!("@{id}"), Style::Mention));
                i += close + 1;
                continue;
            }
        }

        if rest.starts_with("<#") {
            if let Some(close) = plausible_close(rest) {
                flush(&mut plain, out);
                out.push(ctx.segment(format!("#{}", &rest[2..close]), Style::ChannelName));
                i += close + 1;
                continue;
            }
        }

        if rest.starts_with("<:") || rest.starts_with("<a:") {
            if let Some(close) = plausible_close(rest) {
                let prefix_len = if rest.starts_with("<a:") { 3 } else { 2 };
                if rest[prefix_len..close].contains(':') {
                    flush(&mut plain, out);
                    out.push(ctx.segment(&rest[..=close], Style::Emoji));
                    i += close + 1;
                    continue;
                }
            }
        }

        // `<https://…>`: a link without its embed.
        if rest.starts_with('<') && is_web_url(&rest[1..]) {
            if let Some(close) = rest.find('>').filter(|&close| !rest[..close].contains(' ')) {
                flush(&mut plain, out);
                out.push(ctx.segment(&rest[1..close], Style::Link));
                i += close + 1;
                continue;
            }
        }

        // `[text](https://…)`: a masked link.
        if rest.starts_with('[') {
            let masked = rest.find("](").and_then(|mid| {
                let label = &rest[1..mid];
                let target = &rest[mid + 2..];
                let close = target.find(')')?;
                let url = &target[..close];
                (!label.is_empty()
                    && !label.contains(['[', ']', '\n'])
                    && is_web_url(url)
                    && !url.contains(char::is_whitespace))
                .then(|| (label, url, mid + 2 + close + 1))
            });
            if let Some((label, url, len)) = masked {
                flush(&mut plain, out);
                let mut segment = ctx.segment(label, Style::Link);
                segment.url = Some(url.to_string());
                out.push(segment);
                i += len;
                continue;
            }
        }

        if is_web_url(rest) {
            flush(&mut plain, out);
            let end = rest
                .find(|c: char| c.is_whitespace() || c == '>' || c == ')')
                .unwrap_or(rest.len());
            // Trailing punctuation ends the sentence, not the link.
            let url = rest[..end].trim_end_matches(['.', ',', ':', ';', '!', '?', '"', '\'']);
            out.push(ctx.segment(url, Style::Link));
            i += url.len();
            continue;
        }

        let ch = rest.chars().next().expect("rest is non-empty");
        plain.push(ch);
        i += ch.len_utf8();
    }
    flush(&mut plain, out);
}

/// Quotes and code blocks are laid out as blocks, so the line breaks
/// around them would add blank lines: drop one on each side.
fn trim_block_edges(segments: &mut Vec<Segment>) {
    let block = |segment: &Segment| (segment.quote, segment.style == Style::CodeBlock);
    for index in 1..segments.len() {
        if block(&segments[index - 1]) == block(&segments[index]) {
            continue;
        }
        if segments[index - 1].style != Style::CodeBlock && segments[index - 1].text.ends_with('\n')
        {
            segments[index - 1].text.pop();
        }
        if segments[index].style != Style::CodeBlock && segments[index].text.starts_with('\n') {
            segments[index].text.remove(0);
        }
    }
    segments.retain(|segment| !segment.text.is_empty());
}

#[cfg(test)]
mod tests {
    use super::*;

    fn styles(text: &str) -> Vec<(String, Style)> {
        tokenize(text)
            .into_iter()
            .map(|segment| (segment.text, segment.style))
            .collect()
    }

    fn marks(text: &str) -> Vec<(String, Marks)> {
        tokenize(text)
            .into_iter()
            .map(|segment| (segment.text, segment.marks))
            .collect()
    }

    const BOLD: Marks = Marks {
        bold: true,
        italic: false,
        underline: false,
        strike: false,
        spoiler: false,
    };
    const ITALIC: Marks = Marks {
        italic: true,
        ..PLAIN
    };
    const PLAIN: Marks = Marks {
        bold: false,
        italic: false,
        underline: false,
        strike: false,
        spoiler: false,
    };

    #[test]
    fn plain_text_stays_plain() {
        assert_eq!(
            styles("olá mundo"),
            vec![("olá mundo".to_string(), Style::Normal)]
        );
    }

    #[test]
    fn emphasis_nests_and_combines() {
        assert_eq!(
            marks("**oi** e *voce*"),
            vec![
                ("oi".into(), BOLD),
                (" e ".into(), PLAIN),
                ("voce".into(), ITALIC),
            ]
        );
        assert_eq!(
            marks("***tudo***"),
            vec![(
                "tudo".into(),
                Marks {
                    italic: true,
                    ..BOLD
                }
            )]
        );
        assert_eq!(
            marks("__sub__ ~~risco~~ ||segredo||"),
            vec![
                (
                    "sub".into(),
                    Marks {
                        underline: true,
                        ..PLAIN
                    }
                ),
                (" ".into(), PLAIN),
                (
                    "risco".into(),
                    Marks {
                        strike: true,
                        ..PLAIN
                    }
                ),
                (" ".into(), PLAIN),
                (
                    "segredo".into(),
                    Marks {
                        spoiler: true,
                        ..PLAIN
                    }
                ),
            ]
        );
        assert_eq!(
            marks("**a _b_**"),
            vec![
                ("a ".into(), BOLD),
                (
                    "b".into(),
                    Marks {
                        italic: true,
                        ..BOLD
                    }
                ),
            ]
        );
    }

    #[test]
    fn snake_case_escapes_and_loose_stars_stay_plain() {
        assert_eq!(
            marks("um_nome_qualquer"),
            vec![("um_nome_qualquer".into(), PLAIN)]
        );
        assert_eq!(marks(r"\*nao\*"), vec![("*nao*".into(), PLAIN)]);
        assert_eq!(marks("2 * 3 * 4"), vec![("2 * 3 * 4".into(), PLAIN)]);
    }

    #[test]
    fn inline_code_and_fence() {
        assert_eq!(
            styles("`cargo run`"),
            vec![("cargo run".to_string(), Style::Code)]
        );
        assert_eq!(
            styles("antes\n```rust\nfn main() {}\n```\ndepois"),
            vec![
                ("antes".to_string(), Style::Normal),
                ("fn main() {}".to_string(), Style::CodeBlock),
                ("depois".to_string(), Style::Normal),
            ]
        );
        // Markup inside code stays literal.
        assert_eq!(styles("`**x**`"), vec![("**x**".to_string(), Style::Code)]);
    }

    #[test]
    fn unterminated_markers_stay_plain() {
        assert_eq!(
            styles("isso **nao fecha"),
            vec![("isso **nao fecha".to_string(), Style::Normal)]
        );
    }

    #[test]
    fn headings_subtext_and_lists() {
        let sizes: Vec<(String, Size)> = tokenize("# Título\n-# nota\n## b\n### c")
            .into_iter()
            .map(|segment| (segment.text, segment.size))
            .collect();
        assert_eq!(
            sizes,
            vec![
                ("Título".into(), Size::H1),
                ("\n".into(), Size::Normal),
                ("nota".into(), Size::Small),
                ("\n".into(), Size::Normal),
                ("b".into(), Size::H2),
                ("\n".into(), Size::Normal),
                ("c".into(), Size::H3),
            ]
        );
        let text: String = tokenize("- um\n  * dois\n3. tres\n#semespaco")
            .into_iter()
            .map(|segment| segment.text)
            .collect();
        assert_eq!(text, " • um\n     ◦ dois\n 3. tres\n#semespaco");
    }

    #[test]
    fn quotes_group_lines_and_drop_their_edge_breaks() {
        let quoted: Vec<(String, bool)> = tokenize("oi\n> citado\n> **dois**\nfora")
            .into_iter()
            .map(|segment| (segment.text, segment.quote))
            .collect();
        assert_eq!(
            quoted,
            vec![
                ("oi".into(), false),
                ("citado".into(), true),
                ("\n".into(), true),
                ("dois".into(), true),
                ("fora".into(), false),
            ]
        );
        assert!(
            tokenize(">>> tudo\ncitado")
                .iter()
                .all(|segment| segment.quote)
        );
    }

    #[test]
    fn mention_channel_emoji_tokens() {
        assert_eq!(
            styles("oi <@!12345> ve <#98765> e <:sob:123>"),
            vec![
                ("oi ".to_string(), Style::Normal),
                ("@12345".to_string(), Style::Mention),
                (" ve ".to_string(), Style::Normal),
                ("#98765".to_string(), Style::ChannelName),
                (" e ".to_string(), Style::Normal),
                ("<:sob:123>".to_string(), Style::Emoji),
            ]
        );
    }

    #[test]
    fn links_bare_bracketed_and_masked() {
        assert_eq!(
            styles("veja https://exemplo.com/a?b=1 fim"),
            vec![
                ("veja ".to_string(), Style::Normal),
                ("https://exemplo.com/a?b=1".to_string(), Style::Link),
                (" fim".to_string(), Style::Normal),
            ]
        );
        assert_eq!(
            styles("em https://exemplo.com."),
            vec![
                ("em ".to_string(), Style::Normal),
                ("https://exemplo.com".to_string(), Style::Link),
                (".".to_string(), Style::Normal),
            ]
        );
        assert_eq!(
            styles("<https://exemplo.com>"),
            vec![("https://exemplo.com".to_string(), Style::Link)]
        );
        let masked = tokenize("[site](https://exemplo.com) e [x](javascript:alert(1))");
        assert_eq!(masked[0].text, "site");
        assert_eq!(masked[0].url.as_deref(), Some("https://exemplo.com"));
        assert!(
            masked[1..]
                .iter()
                .all(|segment| segment.style == Style::Normal)
        );
    }
}
