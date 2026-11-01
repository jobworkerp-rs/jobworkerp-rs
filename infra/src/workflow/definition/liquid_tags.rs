//! Recognizes Liquid tags the way the Liquid parser does, for code that has to
//! inspect a template without rendering it (e.g. finding secret references).
//!
//! The rules mirror liquid-core's grammar (`grammar.pest`): text is split into
//! elements left to right; at `{{` / `{%` the tag grammar is tried and, if it
//! does not match, a single character is taken as plain text. Bodies of
//! `{% raw %}` and `{% comment %}` blocks are plain text, not tags.

/// Bodies of the tags that Liquid evaluates, without the delimiters and
/// whitespace-control dashes.
///
/// Text that only looks like a tag (inside a string literal, a raw or comment
/// block, or not matching the grammar) is skipped. Each character is examined
/// a bounded number of times, so the cost is linear in the template length.
pub fn liquid_tag_bodies(template: &str) -> Vec<&str> {
    let mut bodies = Vec::new();
    let mut skip = BlockSkip::None;
    let mut pos = 0;
    while let Some(rel) = template[pos..].find('{') {
        let start = pos + rel;
        let Some(tag) = parse_element(template, start) else {
            pos = start + 1;
            continue;
        };
        pos = tag.end;
        skip = match skip {
            BlockSkip::None => match tag.bare_tag_name {
                Some("raw") => BlockSkip::Raw { in_comment: 0 },
                Some("comment") => BlockSkip::Comment { depth: 1 },
                _ => {
                    bodies.push(tag.body);
                    BlockSkip::None
                }
            },
            BlockSkip::Raw { in_comment } => match (tag.bare_tag_name, in_comment) {
                (Some("endraw"), 0) => BlockSkip::None,
                (Some("endraw"), depth) => BlockSkip::Comment { depth },
                _ => skip,
            },
            BlockSkip::Comment { depth } => match tag.bare_tag_name {
                // Comment bodies honor nested comments and raw blocks.
                //
                // Known difference: other block tags inside a comment are not
                // tracked. liquid-rust parses them and ignores their errors, so
                // an unclosed one consumes the next `endcomment`. For
                // `{% comment %}{% if true %}{% endcomment %}{{ a }}{% endcomment %}`
                // liquid-rust keeps `{{ a }}` inside the comment while this
                // scanner treats it as live, which can report an unused secret
                // as an undeclared reference. This only affects malformed
                // comment bodies, follows liquid-rust's error recovery rather
                // than the Liquid specification, and errs toward rejecting.
                Some("comment") => BlockSkip::Comment { depth: depth + 1 },
                Some("endcomment") if depth == 1 => BlockSkip::None,
                Some("endcomment") => BlockSkip::Comment { depth: depth - 1 },
                Some("raw") => BlockSkip::Raw { in_comment: depth },
                _ => skip,
            },
        };
    }
    bodies
}

#[derive(Clone, Copy)]
enum BlockSkip {
    None,
    /// Inside `{% raw %}`; `in_comment` is the comment depth to return to.
    Raw {
        in_comment: usize,
    },
    Comment {
        depth: usize,
    },
}

struct Element<'a> {
    body: &'a str,
    /// Name of a `{% name %}` tag that has no arguments. Liquid only treats
    /// block markers (`raw`, `endraw`, `comment`, ...) without arguments as
    /// block boundaries: `{% endraw x %}` is raw text and `{% raw x %}` is
    /// rejected by the raw block, so tags with arguments never match here.
    bare_tag_name: Option<&'a str>,
    end: usize,
}

/// Parse an Expression (`{{ .. }}`) or Tag (`{% .. %}`) starting at `start`.
fn parse_element(src: &str, start: usize) -> Option<Element<'_>> {
    let rest = &src[start..];
    let is_tag = if rest.starts_with("{{") {
        false
    } else if rest.starts_with("{%") {
        true
    } else {
        return None;
    };
    let close = if is_tag { "%}" } else { "}}" };
    let mut p = Parser {
        src: src.as_bytes(),
        pos: start + 2,
    };
    // `{{-` / `{%-` is the whitespace-control form of the start marker.
    p.eat(b"-");
    let body_start = p.pos;
    p.skip_ws();
    let bare_tag_name = if is_tag {
        let name_start = p.pos;
        let (name_end, has_arguments) = p.tag_inner()?;
        (!has_arguments).then(|| &src[name_start..name_end])
    } else {
        p.filter_chain()?;
        None
    };
    p.skip_ws();
    let body_end = p.pos;
    let end = if p.eat(b"-") && p.eat(close.as_bytes()) {
        p.pos
    } else {
        p.pos = body_end;
        if !p.eat(close.as_bytes()) {
            return None;
        }
        p.pos
    };
    Some(Element {
        body: &src[body_start..body_end],
        bare_tag_name,
        end,
    })
}

/// Recursive-descent recognizer for liquid-core's inner grammar. Methods
/// return `None` without restoring `pos`; callers that try alternatives save
/// and restore it, matching PEG ordered choice.
struct Parser<'a> {
    src: &'a [u8],
    pos: usize,
}

impl Parser<'_> {
    fn peek(&self) -> Option<u8> {
        self.src.get(self.pos).copied()
    }

    fn starts_with(&self, s: &[u8]) -> bool {
        self.src[self.pos..].starts_with(s)
    }

    fn eat(&mut self, s: &[u8]) -> bool {
        let matched = self.starts_with(s);
        if matched {
            self.pos += s.len();
        }
        matched
    }

    /// Grammar WHITESPACE: space and newlines (not tabs).
    fn skip_ws(&mut self) {
        while matches!(self.peek(), Some(b' ' | b'\n' | b'\r')) {
            self.pos += 1;
        }
    }

    /// Try `f`; on failure restore the position.
    fn attempt<T>(&mut self, f: impl FnOnce(&mut Self) -> Option<T>) -> Option<T> {
        let saved = self.pos;
        let result = f(self);
        if result.is_none() {
            self.pos = saved;
        }
        result
    }

    /// `-` belongs to an identifier unless it starts a closing delimiter.
    fn identifier_hyphen(&self) -> bool {
        self.peek() == Some(b'-') && !self.starts_with(b"-}}") && !self.starts_with(b"-%}")
    }

    fn identifier(&mut self) -> Option<()> {
        match self.peek() {
            Some(b) if b.is_ascii_alphabetic() || b == b'_' => {}
            _ if self.identifier_hyphen() => {}
            _ => return None,
        }
        self.pos += 1;
        loop {
            match self.peek() {
                Some(b) if b.is_ascii_alphanumeric() || b == b'_' => self.pos += 1,
                _ if self.identifier_hyphen() => self.pos += 1,
                _ => return Some(()),
            }
        }
    }

    fn string_literal(&mut self) -> Option<()> {
        let quote = self.peek().filter(|q| matches!(q, b'"' | b'\''))?;
        let close = self.src[self.pos + 1..].iter().position(|&b| b == quote)?;
        self.pos += close + 2;
        Some(())
    }

    fn digits(&mut self) -> Option<()> {
        let n = self.src[self.pos..]
            .iter()
            .take_while(|b| b.is_ascii_digit())
            .count();
        (n > 0).then(|| self.pos += n)
    }

    fn number(&mut self) -> Option<()> {
        if !self.eat(b"+") {
            self.eat(b"-");
        }
        self.digits()?;
        // FloatLiteral is tried before IntegerLiteral and needs digits after `.`.
        self.attempt(|p| {
            p.eat(b".").then_some(())?;
            p.digits()
        });
        Some(())
    }

    fn literal(&mut self) -> Option<()> {
        // Keyword literals match as prefixes, exactly as the PEG grammar does.
        for keyword in [&b"nil"[..], b"null", b"empty", b"blank"] {
            if self.eat(keyword) {
                return Some(());
            }
        }
        if self.attempt(Self::string_literal).is_some() || self.attempt(Self::number).is_some() {
            return Some(());
        }
        (self.eat(b"true") || self.eat(b"false")).then_some(())
    }

    fn variable(&mut self) -> Option<()> {
        self.identifier()?;
        loop {
            let step = self.attempt(|p| {
                if p.eat(b".") {
                    p.identifier()
                } else if p.eat(b"[") {
                    p.skip_ws();
                    p.value()?;
                    p.skip_ws();
                    p.eat(b"]").then_some(())
                } else {
                    None
                }
            });
            if step.is_none() {
                return Some(());
            }
        }
    }

    fn value(&mut self) -> Option<()> {
        if self.attempt(Self::literal).is_some() {
            return Some(());
        }
        self.variable()
    }

    fn filter_argument(&mut self) -> Option<()> {
        let keyword = self.attempt(|p| {
            p.identifier()?;
            p.skip_ws();
            p.eat(b":").then_some(())?;
            p.skip_ws();
            p.value()
        });
        if keyword.is_some() {
            return Some(());
        }
        self.value()
    }

    fn filter(&mut self) -> Option<()> {
        self.identifier()?;
        self.attempt(|p| {
            p.skip_ws();
            p.eat(b":").then_some(())?;
            p.skip_ws();
            p.filter_argument()?;
            loop {
                let more = p.attempt(|p| {
                    p.skip_ws();
                    p.eat(b",").then_some(())?;
                    p.skip_ws();
                    p.filter_argument()
                });
                if more.is_none() {
                    return Some(());
                }
            }
        });
        Some(())
    }

    fn filter_chain(&mut self) -> Option<()> {
        self.value()?;
        while self
            .attempt(|p| {
                p.skip_ws();
                p.eat(b"|").then_some(())?;
                p.skip_ws();
                p.filter()
            })
            .is_some()
        {}
        Some(())
    }

    fn range(&mut self) -> Option<()> {
        self.eat(b"(").then_some(())?;
        self.skip_ws();
        self.value()?;
        self.skip_ws();
        self.eat(b"..").then_some(())?;
        self.skip_ws();
        self.value()?;
        self.skip_ws();
        self.eat(b")").then_some(())
    }

    fn tag_token(&mut self) -> Option<()> {
        if self.attempt(Self::range).is_some() || self.attempt(Self::filter_chain).is_some() {
            return Some(());
        }
        for symbol in [
            &b"=="[..],
            b"!=",
            b"<>",
            b">=",
            b"<=",
            b">",
            b"<",
            b"=",
            b",",
            b":",
        ] {
            if self.eat(symbol) {
                return Some(());
            }
        }
        None
    }

    /// Returns the end offset of the tag name and whether arguments follow.
    fn tag_inner(&mut self) -> Option<(usize, bool)> {
        self.identifier()?;
        let name_end = self.pos;
        let mut has_arguments = false;
        while self
            .attempt(|p| {
                p.skip_ws();
                p.tag_token()
            })
            .is_some()
        {
            has_arguments = true;
        }
        Some((name_end, has_arguments))
    }
}

#[cfg(test)]
mod tests {
    use super::liquid_tag_bodies;
    use std::time::{Duration, Instant};

    #[test]
    fn extracts_expression_and_tag_bodies() {
        assert_eq!(
            liquid_tag_bodies(
                "literal secrets.PUBLIC_PATH {{ secrets.API_TOKEN }} {% assign x = y %}"
            ),
            vec![" secrets.API_TOKEN ", " assign x = y "]
        );
        assert!(liquid_tag_bodies("literal secrets.PUBLIC_PATH").is_empty());
        assert_eq!(
            liquid_tag_bodies("{% for i in (1..n) %}{{ i | plus: 1, base: 2 }}{% endfor %}"),
            vec![" for i in (1..n) ", " i | plus: 1, base: 2 ", " endfor "]
        );
    }

    #[test]
    fn delimiters_and_tags_inside_string_literals_are_not_tags() {
        assert_eq!(
            liquid_tag_bodies(r#"{{ "}}" | append: name }}"#),
            vec![r#" "}}" | append: name "#]
        );
        assert_eq!(
            liquid_tag_bodies(r#"{{ "{{ secrets.UNUSED }}" }}"#),
            vec![r#" "{{ secrets.UNUSED }}" "#]
        );
        assert_eq!(
            liquid_tag_bodies("{% assign x = '%}' %}{{ x }}"),
            vec![" assign x = '%}' ", " x "]
        );
        // Quotes outside tags are plain text.
        assert_eq!(liquid_tag_bodies(r#"say "hi {{ name }}"#), vec![" name "]);
    }

    #[test]
    fn whitespace_control_dashes_are_not_part_of_the_body() {
        assert_eq!(liquid_tag_bodies("{{-name-}}"), vec!["name"]);
        assert_eq!(liquid_tag_bodies("{{ -name-}}"), vec![" -name"]);
        assert_eq!(liquid_tag_bodies("{{--name-}}"), vec!["-name"]);
        assert_eq!(liquid_tag_bodies("{{- -name -}}"), vec![" -name "]);
        assert_eq!(liquid_tag_bodies("{%-if x-%}"), vec!["if x"]);
        assert_eq!(liquid_tag_bodies("{{ name- }}"), vec![" name- "]);
    }

    #[test]
    fn text_not_matching_the_grammar_is_plain_text() {
        // An unbalanced quote or trailing junk is not a tag; scanning resumes
        // one character later, as Liquid does, so later tags are found.
        assert_eq!(liquid_tag_bodies(r#"{{ " }}{{ name }}"#), vec![" name "]);
        assert_eq!(
            liquid_tag_bodies(r#"{{ "x" junk }}{{ name }}"#),
            vec![" name "]
        );
        assert!(liquid_tag_bodies(r#"{{ " "#).is_empty());
        // Tabs are not grammar whitespace.
        assert!(liquid_tag_bodies("{{\tname }}").is_empty());
    }

    #[test]
    fn raw_and_comment_bodies_are_skipped() {
        assert_eq!(
            liquid_tag_bodies("{% raw %}{{ secrets.X }}{% endraw %}{{ y }}"),
            vec![" y "]
        );
        assert_eq!(
            liquid_tag_bodies(r#"{% raw %}{{ "{% endraw %}{% if name %}YES{% endif %}"#),
            vec![" if name ", " endif "]
        );
        assert_eq!(
            liquid_tag_bodies(r#"{% raw %}{{ "{% endraw %}{{ name }}" junk }}"#),
            vec![" name "]
        );
        assert_eq!(
            liquid_tag_bodies(
                "{% comment %}{% comment %}{{ a }}{% endcomment %}{{ b }}{% endcomment %}{{ c }}"
            ),
            vec![" c "]
        );
        // A raw block inside a comment hides an `endcomment` in its body.
        assert_eq!(
            liquid_tag_bodies(
                "{% comment %}{% raw %}{% endcomment %}{% endraw %}{{ a }}{% endcomment %}{{ b }}"
            ),
            vec![" b "]
        );
        assert_eq!(
            liquid_tag_bodies("{%- comment -%}{{ a }}{%- endcomment -%}{{ b }}"),
            vec![" b "]
        );
    }

    // Each case also renders the template with liquid itself, pinning the
    // block-boundary rule the scanner mirrors: block markers with arguments
    // are not boundaries (`{% endraw x %}` is raw text, and `{% raw x %}` is
    // rejected by the raw block so it starts nothing inside a comment).
    #[test]
    fn block_markers_with_arguments_are_not_block_boundaries() {
        let parser = liquid::ParserBuilder::with_stdlib().build().unwrap();
        let globals = liquid::object!({ "secrets": { "API_TOKEN": "t" } });
        for (template, bodies, rendered) in [
            (
                "{% raw %}{% endraw x %}{{ secrets.UNUSED }}{% endraw %}",
                vec![],
                "{% endraw x %}{{ secrets.UNUSED }}",
            ),
            (
                "{% comment %}{% raw x %}{% endcomment %}{{ secrets.API_TOKEN }}",
                vec![" secrets.API_TOKEN "],
                "t",
            ),
        ] {
            assert_eq!(liquid_tag_bodies(template), bodies, "{template}");
            let output = parser.parse(template).unwrap().render(&globals).unwrap();
            assert_eq!(output, rendered, "{template}");
        }
    }

    #[test]
    fn many_openers_are_scanned_in_linear_time() {
        let template = format!(
            "{{% raw %}}{}{{% endraw %}}{{{{ x }}}}",
            "{{ \"".repeat(50_000)
        );
        let started = Instant::now();
        assert_eq!(liquid_tag_bodies(&template), vec![" x "]);
        assert!(started.elapsed() < Duration::from_secs(2));
    }
}
