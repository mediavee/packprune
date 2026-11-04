//! The 26.2 client's assembly of a shader source, ported for tests: `ShaderManager.loadShader` runs
//! `GlslPreprocessor.process` and joins its pieces, then `GlDevice.compileShader` (offsets 42-68) runs `injectDefines`
//! before `glShaderSource`. Offsets are those of `javap -c` on the 26.2 jar. Relative imports are not ported: their
//! path goes through `FileUtil.getFullResourcePath`.

use std::collections::BTreeMap;
use std::sync::LazyLock;

use packprune_codec::glsl;
use regex::{Captures, Regex};

// Java's `\h` and `\v` over the lexer's ASCII; `.` does not match `\r` in Java.
static MOJ_IMPORT: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(#(?:/\*(?:[^*]|\*+[^*/])*\*+/|[ \t])*moj_import(?:/\*(?:[^*]|\*+[^*/])*\*+/|[ \t])*(?:"([^\n\r]*)"|<([^\n\r]*)>))"#)
        .unwrap()
});
static VERSION: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(#(?:/\*(?:[^*]|\*+[^*/])*\*+/|[ \t])*version(?:/\*(?:[^*]|\*+[^*/])*\*+/|[ \t])*(\d+))\b")
        .unwrap()
});
static ENDS_WITH_WHITESPACE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?:^|[\n\x0B\x0C\r])(?:\s|/\*(?:[^*]|\*+[^*/])*\*+/|(//[^\n\x0B\x0C\r]*))*\z")
        .unwrap()
});
static LINE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\r\n|[\n\x0B\x0C\r]").unwrap());

#[derive(Default)]
struct Context {
    glsl_version: u32,
    source_id: u32,
}

/// What `glShaderSource` receives for `main`, its `<name>` imports read from `includes`.
pub(crate) fn client_source(
    main: &str,
    includes: &BTreeMap<&str, &str>,
    defines: &[(&str, &str)],
) -> String {
    inject_defines(&process(main, includes).concat(), defines)
}

/// The tokens of `client_source`, each with its line: what the GLSL compiler reads.
pub(crate) fn compiled(source: &str) -> Vec<(String, usize)> {
    glsl::tokenize(source.as_bytes())
        .unwrap()
        .iter()
        .map(|token| {
            (
                String::from_utf8_lossy(token.bytes).into_owned(),
                token.line,
            )
        })
        .collect()
}

/// `process`, offsets 0-44.
fn process(source: &str, includes: &BTreeMap<&str, &str>) -> Vec<String> {
    let mut context = Context::default();
    let mut pieces = process_imports(source, &mut context, includes);
    pieces[0] = set_version(&pieces[0], context.glsl_version);
    pieces
}

/// `processImports`, offsets 0-433.
fn process_imports(
    source: &str,
    context: &mut Context,
    includes: &BTreeMap<&str, &str>,
) -> Vec<String> {
    let source_id = context.source_id;
    let mut last = 0;
    let mut prefix = String::new();
    let mut out = Vec::new();
    for found in MOJ_IMPORT.captures_iter(source) {
        if directive_disabled(source, &found, last) {
            continue;
        }
        let (relative, name) = match (found.get(2), found.get(3)) {
            (Some(name), _) => (true, name.as_str()),
            (None, Some(name)) => (false, name.as_str()),
            _ => continue,
        };
        assert!(!relative, "relative imports are not ported");
        let whole = found.get(1).unwrap();
        let before = &source[last..whole.start()];
        let content = includes.get(name).copied().unwrap_or_default();
        if !content.is_empty() {
            // Offsets 132-150: `System.lineSeparator()`, LF on the servers' and players' Unix hosts.
            let content = if ends_with_new_line(content) {
                content.to_owned()
            } else {
                format!("{content}\n")
            };
            context.source_id += 1;
            let id = context.source_id;
            let mut pieces = process_imports(&content, context, includes);
            let first = process_versions(&pieces[0], context);
            pieces[0] = format!("#line 0 {id}\n{first}");
            if !is_blank(before) {
                out.push(before.to_owned());
            }
            out.extend(pieces);
        } else {
            out.push(format!("{prefix}{before}/*#moj_import <{name}>*/"));
        }
        let line = line_count(&source[..whole.end()]);
        prefix = format!("#line {line} {source_id}");
        last = whole.end();
    }
    let tail = &source[last..];
    if !is_blank(tail) {
        out.push(format!("{prefix}{tail}"));
    }
    out
}

/// `processVersions`, offsets 0-82: an import's `#version` raises the context's and is commented out.
fn process_versions(source: &str, context: &mut Context) -> String {
    match VERSION.captures(source) {
        Some(found) if !directive_disabled(source, &found, 0) => {
            let version: u32 = found[2].parse().unwrap();
            context.glsl_version = context.glsl_version.max(version);
            let whole = found.get(1).unwrap();
            format!(
                "{}/*{}*/{}",
                &source[..whole.start()],
                whole.as_str(),
                &source[whole.end()..]
            )
        }
        _ => source.to_owned(),
    }
}

/// `setVersion`, offsets 0-61.
fn set_version(source: &str, version: u32) -> String {
    match VERSION.captures(source) {
        Some(found) if !directive_disabled(source, &found, 0) => {
            let number = found.get(2).unwrap();
            let written: u32 = number.as_str().parse().unwrap();
            format!(
                "{}{}{}",
                &source[..number.start()],
                version.max(written),
                &source[number.end()..]
            )
        }
        _ => source.to_owned(),
    }
}

/// `isDirectiveDisabled`, offsets 0-62. As the client does, the line comment's end, relative to `last`, is compared
/// with the match's start in the whole source.
fn directive_disabled(source: &str, found: &Captures<'_>, last: usize) -> bool {
    let start = found.get(0).unwrap().start();
    if start == last {
        return false;
    }
    match ENDS_WITH_WHITESPACE.captures(&source[last..start]) {
        None => true,
        Some(space) => space.get(1).is_some_and(|comment| comment.end() == start),
    }
}

/// `injectDefines`, offsets 0-40: after the first LF, or at the start without one.
fn inject_defines(source: &str, defines: &[(&str, &str)]) -> String {
    if defines.is_empty() {
        return source.to_owned();
    }
    let at = source.find('\n').map_or(0, |found| found + 1);
    // `ShaderDefines.asSourceDirectives`.
    let directives: String = defines
        .iter()
        .map(|(name, value)| format!("#define {name} {value}\n"))
        .collect();
    format!("{}{directives}#line 1 0\n{}", &source[..at], &source[at..])
}

/// `StringUtil.lineCount`.
fn line_count(text: &str) -> usize {
    if text.is_empty() {
        0
    } else {
        1 + LINE.find_iter(text).count()
    }
}

/// `StringUtil.endsWithNewLine`: `(?:\r\n|\v)$`.
fn ends_with_new_line(text: &str) -> bool {
    text.ends_with(['\n', '\x0B', '\x0C', '\r'])
}

/// `StringUtil.isBlank`.
fn is_blank(text: &str) -> bool {
    text.chars().all(char::is_whitespace)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Each case's files minified, then assembled by the client with defines: the compiler reads the same tokens on
    /// the same lines.
    fn assert_minifying_keeps(main: &str, includes: &[(&str, &str)]) {
        let defines = [("ALPHA_CUTOUT", "0.1")];
        let before: BTreeMap<&str, &str> = includes.iter().copied().collect();
        let minified: Vec<(String, String)> = includes
            .iter()
            .map(|(name, text)| {
                let out = glsl::minify(text.as_bytes()).unwrap();
                ((*name).to_owned(), String::from_utf8(out).unwrap())
            })
            .collect();
        let after: BTreeMap<&str, &str> = minified
            .iter()
            .map(|(name, text)| (name.as_str(), text.as_str()))
            .collect();
        let main_after = String::from_utf8(glsl::minify(main.as_bytes()).unwrap()).unwrap();
        assert_eq!(
            compiled(&client_source(&main_after, &after, &defines)),
            compiled(&client_source(main, &before, &defines)),
            "{main}"
        );
    }

    /// A block comment open across the first LF holds the injected defines and `#line 1 0`: removing it makes them
    /// active before `#version`, and the shader no longer compiles.
    #[test]
    fn injected_defines_land_where_they_did() {
        assert_minifying_keeps(
            "/* header\n   header continued */\n#version 330 core\n#ifndef ALPHA_CUTOUT\n#define ALPHA_CUTOUT 0.25\n#endif\nout vec4 fragColor;\nvoid main() { fragColor = vec4(ALPHA_CUTOUT); }\n",
            &[],
        );
        assert_minifying_keeps(
            "// header\n#version 330 core\n#ifndef ALPHA_CUTOUT\n#define ALPHA_CUTOUT 0.25\n#endif\nout vec4 fragColor; // colour\nvoid main() { fragColor = vec4(ALPHA_CUTOUT); }\n",
            &[],
        );
    }

    /// Imports that number `__LINE__` and `__FILE__`, through the client's own assembly.
    #[test]
    fn imports_assemble_as_they_did() {
        assert_minifying_keeps(
            "#version 330 core\n#moj_import <line_number.glsl>\n\nout vec4 fragColor;\n\nvoid main() {\n    fragColor = vec4(float(LINE_NUMBER) / 255.0);\n}\n",
            &[("line_number.glsl", "#define LINE_NUMBER __LINE__\n")],
        );
        assert_minifying_keeps(
            "#version 330 core\n#moj_import <empty.glsl>\n#moj_import <source.glsl>\nout vec4 fragColor;\nvoid main() {\n    fragColor = vec4(float(IMPORT_ID) / 255.0);\n}\n",
            &[
                (
                    "empty.glsl",
                    "/* intentionally empty\n   multiline include\n*/\n",
                ),
                ("source.glsl", "const int IMPORT_ID = __FILE__;   \n\n\n"),
            ],
        );
        assert_minifying_keeps(
            "#moj_import <a.glsl>\n// between\nfloat x = __LINE__;\n#moj_import <b.glsl>\nfloat y = __LINE__; // after\n",
            &[
                ("a.glsl", "#version 150\n/* a */ float a = __LINE__;\n\n"),
                ("b.glsl", "float b = __FILE__; // b\nfloat c = __LINE__;"),
            ],
        );
    }
}
