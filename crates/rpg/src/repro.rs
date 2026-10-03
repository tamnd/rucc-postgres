//! `rpg repro`: everything a compiler bug report needs about one translation unit of a build.
//!
//! A finished build keeps one record per compiler call in `compile.jsonl`, with the argv, the
//! working directory and the compiler that actually ran. This command finds the one call that
//! compiled a given file and writes a bundle directory from it:
//!
//! - `<name>.i`, the preprocessed source, made by running the recorded command again in the
//!   recorded directory with the recorded compiler, with `-E -o <bundle>/<name>.i` in place of
//!   `-c -o <object>` and without the options that write dependency files;
//! - `command.txt`, the recorded command line, working directory and environment;
//! - `compile.sh`, which compiles the `.i` again with the recorded flags minus the ones that only
//!   matter to the preprocessor, so it runs anywhere the compiler does;
//! - `compiler.txt`, what the compiler says for `--version`, and the commit of the checkout it
//!   was built in when it sits in one, since `rucc --version` names a release and not a commit.
//!
//! The file is named relative to the Postgres source tree, as `src/backend/parser/gram.c`. A
//! generated file, which meson writes into the build tree, is named the same way relative to the
//! build tree. When no call compiled the file, or more than one did, the command says so and
//! lists what it found; `--object` picks one of several by the object it wrote.

use crate::build::{BuildInfo, lexical};
use crate::compiler::checkout_commit;
use crate::process::{canonical, capture, shell_quote};
use rpg_shim::args::{self, Mode, split_response_file};
use rpg_shim::record::{CompileRecord, read_log};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::Command;

/// The name a record's C input is known by: relative to the first root it falls under, or as it
/// was written when it is under none of them.
pub fn names(record: &CompileRecord, roots: &[&Path]) -> Vec<(String, String)> {
    record
        .inputs
        .iter()
        .filter(|d| args::is_c_source(&d.path))
        .map(|d| {
            let full = lexical(&Path::new(&record.cwd).join(&d.path));
            let name = roots
                .iter()
                .find_map(|root| full.strip_prefix(root).ok())
                .map_or_else(|| full.display().to_string(), |p| p.display().to_string());
            (name, d.path.clone())
        })
        .collect()
}

/// A compile of the wanted file: the record, and the input path as the command line wrote it.
#[derive(Debug, Clone)]
pub struct Found<'a> {
    /// The call.
    pub record: &'a CompileRecord,
    /// The argument that named the file.
    pub input: String,
    /// The object it wrote, when it said.
    pub object: Option<String>,
}

/// Every call that compiled `wanted` to an object.
///
/// `wanted` is relative to one of `roots`, or absolute. Calls that preprocess or link are left
/// out, so a test program built from one C file in one step does not count as a compile of it.
#[must_use]
pub fn find<'a>(records: &'a [CompileRecord], wanted: &str, roots: &[&Path]) -> Vec<Found<'a>> {
    let wanted = lexical(Path::new(wanted)).display().to_string();
    let mut out = Vec::new();
    for record in records {
        let rest = record.argv.get(1..).unwrap_or_default();
        let invocation = args::read(rest, Path::new(&record.cwd), &|_| true);
        if invocation.mode != Mode::Compile {
            continue;
        }
        for (name, input) in names(record, roots) {
            if name == wanted {
                out.push(Found {
                    record,
                    input,
                    object: invocation.outputs.first().cloned(),
                });
            }
        }
    }
    out
}

/// Options that take the next argument as their value, among those this module drops.
const DROP_WITH_VALUE: &[&str] = &[
    "-o",
    "-MF",
    "-MT",
    "-MQ",
    "-I",
    "-D",
    "-U",
    "-include",
    "-imacros",
    "-isystem",
    "-iquote",
    "-idirafter",
    "-iprefix",
    "-iwithprefix",
    "-iwithprefixbefore",
    "-isysroot",
    "--sysroot",
    "-x",
    "-Xpreprocessor",
];

/// Options that only concern writing dependency files, which a rerun must not touch.
pub fn is_dependency_option(arg: &str) -> bool {
    matches!(
        arg,
        "-MD" | "-MMD" | "-MP" | "-M" | "-MM" | "-MG" | "-fpch-deps"
    ) || ["-MF", "-MT", "-MQ"]
        .iter()
        .any(|o| arg.starts_with(o) && arg.len() > o.len())
}

/// Options that only matter to the preprocessor, beyond the dependency ones.
fn is_preprocessor_option(arg: &str) -> bool {
    const JOINED: &[&str] = &[
        "-I",
        "-D",
        "-U",
        "-isystem",
        "-iquote",
        "-idirafter",
        "-iprefix",
        "-iwithprefix",
        "-isysroot",
        "--sysroot=",
        "-Wp,",
    ];
    matches!(
        arg,
        "-nostdinc" | "-undef" | "-C" | "-CC" | "-P" | "-H" | "-trigraphs" | "-Winvalid-pch"
    ) || is_dependency_option(arg)
        || JOINED.iter().any(|o| arg.starts_with(o))
}

/// Expand `@file` arguments, read relative to `cwd`, leaving any that cannot be read as they are.
fn expand(args: &[String], cwd: &Path) -> Vec<String> {
    let mut out = Vec::new();
    for arg in args {
        if let Some(name) = arg.strip_prefix('@')
            && let Ok(text) = std::fs::read_to_string(cwd.join(name))
        {
            out.extend(split_response_file(&text));
            continue;
        }
        out.push(arg.clone());
    }
    out
}

/// The recorded arguments, minus `-c`, the output and the dependency options, plus `-E -o i`.
#[must_use]
pub fn preprocess_args(rest: &[String], i: &Path) -> Vec<String> {
    let mut out = Vec::new();
    let mut words = rest.iter();
    while let Some(arg) = words.next() {
        if matches!(arg.as_str(), "-o" | "-MF" | "-MT" | "-MQ") {
            words.next();
            continue;
        }
        if arg == "-c" || (arg.starts_with("-o") && arg.len() > 2) || is_dependency_option(arg) {
            continue;
        }
        out.push(arg.clone());
    }
    out.extend(["-E".to_string(), "-o".to_string(), i.display().to_string()]);
    out
}

/// The recorded arguments with the source replaced by the `.i` and the preprocessor options gone.
#[must_use]
pub fn compile_args(rest: &[String], input: &str, i: &str, object: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut words = rest.iter();
    while let Some(arg) = words.next() {
        if DROP_WITH_VALUE.contains(&arg.as_str()) {
            words.next();
            continue;
        }
        if arg == input
            || arg == "-c"
            || (arg.starts_with("-o") && arg.len() > 2)
            || (arg.starts_with("-x") && arg.len() > 2)
            || is_preprocessor_option(arg)
        {
            continue;
        }
        out.push(arg.clone());
    }
    out.extend([
        "-c".to_string(),
        i.to_string(),
        "-o".to_string(),
        object.to_string(),
    ]);
    out
}

/// A command line as one shell line.
fn shell_line(program: &str, args: &[String]) -> String {
    std::iter::once(program)
        .chain(args.iter().map(String::as_str))
        .map(shell_quote)
        .collect::<Vec<_>>()
        .join(" ")
}

/// The text of `command.txt`.
#[must_use]
pub fn command_text(record: &CompileRecord, file: &str) -> String {
    let rest = record.argv.get(1..).unwrap_or_default();
    let mut text = format!("# The compile of {file} as the build ran it, from compile.jsonl.\n\n");
    let _ = writeln!(text, "directory: {}", record.cwd);
    let _ = writeln!(text, "compiler:  {}", record.compiler);
    let exit = match (record.exit, record.signal) {
        (Some(code), _) => format!("exited {code}"),
        (None, Some(signal)) => format!("killed by signal {signal}"),
        (None, None) => "unknown".to_string(),
    };
    let _ = writeln!(text, "status:    {exit}");
    if !record.added.is_empty() {
        let _ = writeln!(
            text,
            "added:     {} (by the shim, left out below)",
            record.added.join(" ")
        );
    }
    for (name, value) in &record.env {
        let _ = writeln!(text, "env:       {name}={value}");
    }
    let _ = writeln!(text, "\ncd {}", shell_quote(&record.cwd));
    let _ = writeln!(text, "{}", shell_line(&record.compiler, rest));
    if !record.stderr.is_empty() {
        let _ = writeln!(text, "\nstandard error, first KiB:\n{}", record.stderr);
    }
    text
}

/// The text of `compile.sh`.
#[must_use]
pub fn script_text(compiler: &str, args: &[String], file: &str) -> String {
    format!(
        "#!/bin/sh\n\
         # Compile the preprocessed {file} again with the flags the build used,\n\
         # less the ones that only matter to the preprocessor. Written by rpg repro.\n\
         # CC overrides the compiler, and extra arguments are passed on.\n\
         set -e\n\
         cd \"$(dirname \"$0\")\"\n\
         exec \"${{CC:-{}}}\" {} \"$@\"\n",
        compiler.replace(['"', '$', '`', '\\'], ""),
        args.iter()
            .map(|a| shell_quote(a))
            .collect::<Vec<_>>()
            .join(" ")
    )
}

/// The file name a bundle's files are called by: `gram` for `src/backend/parser/gram.c`.
fn stem(file: &str) -> String {
    Path::new(file)
        .file_stem()
        .map_or_else(|| "unit".to_string(), |s| s.to_string_lossy().into_owned())
}

/// The default bundle directory: `repro/src-backend-parser-gram` inside the build directory.
#[must_use]
pub fn default_out(build: &Path, file: &str) -> PathBuf {
    let name = file.trim_end_matches(".c").replace('/', "-");
    build.join("repro").join(name)
}

/// What `rpg repro` needs.
pub struct Request<'a> {
    /// The build directory, with `build.json` and `compile.jsonl`.
    pub build: &'a Path,
    /// The file, relative to the source tree.
    pub file: &'a str,
    /// Where to write the bundle.
    pub out: &'a Path,
    /// Which of several compiles, by a part of the object path.
    pub object: Option<&'a str>,
}

/// What was written.
pub struct Bundle {
    /// The bundle directory.
    pub dir: PathBuf,
    /// The `.i`, when preprocessing worked.
    pub preprocessed: Option<PathBuf>,
    /// Why preprocessing did not work, when it did not.
    pub preprocess_error: Option<String>,
    /// The first line of the compiler's `--version`.
    pub compiler: String,
}

/// Pick the one compile of the file, or say why there is not one.
fn pick<'a>(found: Vec<Found<'a>>, file: &str, object: Option<&str>) -> Result<Found<'a>, String> {
    let mut found: Vec<Found<'a>> = match object {
        Some(part) => found
            .into_iter()
            .filter(|f| f.object.as_deref().is_some_and(|o| o.contains(part)))
            .collect(),
        None => found,
    };
    match found.len() {
        1 => Ok(found.remove(0)),
        0 => Err(format!(
            "no compile of {file} in the records{}",
            object.map_or_else(String::new, |o| format!(" with an object matching {o}"))
        )),
        n => {
            let list: Vec<String> = found
                .iter()
                .map(|f| {
                    format!(
                        "{} in {}",
                        f.object.as_deref().unwrap_or("(no object named)"),
                        f.record.cwd
                    )
                })
                .collect();
            Err(format!(
                "{n} compiles of {file} in the records, pick one with --object:\n  {}",
                list.join("\n  ")
            ))
        }
    }
}

/// Write the bundle.
pub fn repro(request: &Request) -> Result<Bundle, String> {
    let info = BuildInfo::load(request.build)?;
    let log = request.build.join("compile.jsonl");
    let (records, _) = read_log(&log).map_err(|e| format!("reading {}: {e}", log.display()))?;
    let source = PathBuf::from(&info.source);
    let build_dir = PathBuf::from(&info.build_dir);
    let found = find(&records, request.file, &[&source, &build_dir]);
    let found = pick(found, request.file, request.object)?;
    let record = found.record;

    std::fs::create_dir_all(request.out)
        .map_err(|e| format!("creating {}: {e}", request.out.display()))?;
    let dir = canonical(request.out).unwrap_or_else(|_| request.out.to_path_buf());
    let write = |name: &str, text: &str| {
        let path = dir.join(name);
        std::fs::write(&path, text).map_err(|e| format!("writing {}: {e}", path.display()))
    };

    write("command.txt", &command_text(record, request.file))?;

    let name = stem(request.file);
    let cwd = Path::new(&record.cwd);
    let rest = record.argv.get(1..).unwrap_or_default();
    let i_name = format!("{name}.i");
    let flags = compile_args(
        &expand(rest, cwd),
        &found.input,
        &i_name,
        &format!("{name}.o"),
    );
    let script = dir.join("compile.sh");
    write(
        "compile.sh",
        &script_text(&record.compiler, &flags, request.file),
    )?;
    make_executable(&script);

    let compiler = Path::new(&record.compiler);
    // A compiler that cannot say what it is still leaves a bundle worth having, so a failure here
    // is written into the file rather than returned.
    let version = capture(compiler, &["--version"]).unwrap_or_else(|e| format!("{e}\n"));
    let mut about = version.clone();
    if let Some(commit) = checkout_commit(compiler) {
        let _ = writeln!(about, "\ncheckout commit: {commit}");
    }
    if let Some(commit) = &info.rucc_commit {
        let _ = writeln!(about, "commit in build.json: {commit}");
    }
    write("compiler.txt", &about)?;

    // The recorded environment is applied except PATH, which puts the shim first; a compiler that
    // ran `cc` for some reason would otherwise add a line to the build's compile.jsonl.
    let i_path = dir.join(&i_name);
    let output = Command::new(compiler)
        .args(preprocess_args(rest, &i_path))
        .current_dir(cwd)
        .envs(record.env.iter().filter(|(k, _)| *k != "PATH"))
        .output();
    let (preprocessed, preprocess_error) = match output {
        Ok(output) if output.status.success() && i_path.is_file() => (Some(i_path), None),
        Ok(output) => {
            let stderr = String::from_utf8_lossy(&output.stderr);
            (
                None,
                Some(format!(
                    "{} -E exited {}: {}",
                    compiler.display(),
                    output.status,
                    stderr.lines().take(5).collect::<Vec<_>>().join("\n")
                )),
            )
        }
        Err(e) => (
            None,
            Some(format!("could not run {}: {e}", compiler.display())),
        ),
    };
    Ok(Bundle {
        dir,
        preprocessed,
        preprocess_error,
        compiler: version.lines().next().unwrap_or_default().to_string(),
    })
}

#[cfg(unix)]
fn make_executable(path: &Path) {
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).ok();
}

#[cfg(not(unix))]
fn make_executable(_path: &Path) {}

#[cfg(test)]
mod tests {
    use super::*;
    use rpg_shim::record::parse_log;

    /// Three calls in the shape server2's rucc build wrote them: a meson compile of a generated
    /// file, a compile of a source from the cached tree, and a link.
    const LOG: &str = r#"{"started":1.0,"argv":["/w/b/bin/cc","-Isrc/backend/parser","-I../../src/REL_18_6/src/include","-D_GNU_SOURCE","-O2","-g","-fwrapv","-MD","-MQ","src/backend/parser/parser.a.p/gram.c.o","-MF","src/backend/parser/parser.a.p/gram.c.o.d","-o","src/backend/parser/parser.a.p/gram.c.o","-c","src/backend/parser/gram.c"],"compiler":"/opt/rucc/target/release/rucc","added":["-frucc-trace=/tmp/t.jsonl"],"cwd":"/w/b/build","env":{"LANG":"C.UTF-8","PATH":"/w/b/bin:/usr/bin"},"inputs":[{"path":"src/backend/parser/gram.c","sha256":"a"}],"outputs":[{"path":"src/backend/parser/parser.a.p/gram.c.o","sha256":"b"}],"wall-seconds":9.8,"exit":0}
{"started":2.0,"argv":["/w/b/bin/cc","-Isrc/include","-include","pg_config.h","-DFRONTEND","-O2","-o","src/port/libpgport.a.p/path.c.o","-c","../../src/REL_18_6/src/port/path.c"],"compiler":"/opt/rucc/target/release/rucc","cwd":"/w/b/build","inputs":[{"path":"../../src/REL_18_6/src/port/path.c","sha256":"c"}],"wall-seconds":0.1,"exit":0}
{"started":3.0,"argv":["/w/b/bin/cc","-Isrc/include","-O2","-fPIC","-o","src/port/libpgport_shlib.a.p/path.c.o","-c","../../src/REL_18_6/src/port/path.c"],"compiler":"/opt/rucc/target/release/rucc","cwd":"/w/b/build","inputs":[{"path":"../../src/REL_18_6/src/port/path.c","sha256":"c"}],"wall-seconds":0.1,"exit":0}
{"started":4.0,"argv":["/w/b/bin/cc","-o","src/test/t","../../src/REL_18_6/src/test/t.c"],"compiler":"/opt/rucc/target/release/rucc","cwd":"/w/b/build","inputs":[{"path":"../../src/REL_18_6/src/test/t.c","sha256":"d"}],"wall-seconds":0.1,"exit":0}
"#;

    fn roots() -> [&'static Path; 2] {
        [Path::new("/w/src/REL_18_6"), Path::new("/w/b/build")]
    }

    #[test]
    fn a_file_is_found_under_the_source_tree_or_the_build_tree() {
        let (records, _) = parse_log(LOG);
        let gram = find(&records, "src/backend/parser/gram.c", &roots());
        assert_eq!(gram.len(), 1);
        assert_eq!(gram[0].input, "src/backend/parser/gram.c");
        let path = find(&records, "./src/port/path.c", &roots());
        assert_eq!(path.len(), 2);
        assert!(find(&records, "src/test/t.c", &roots()).is_empty());
        assert!(find(&records, "src/nowhere.c", &roots()).is_empty());
    }

    #[test]
    fn none_or_several_are_refused_and_listed() {
        let (records, _) = parse_log(LOG);
        let several = pick(
            find(&records, "src/port/path.c", &roots()),
            "src/port/path.c",
            None,
        );
        let message = several.err().unwrap();
        assert!(message.contains("2 compiles"));
        assert!(message.contains("libpgport.a.p/path.c.o"));
        assert!(message.contains("libpgport_shlib.a.p/path.c.o"));
        let one = pick(
            find(&records, "src/port/path.c", &roots()),
            "src/port/path.c",
            Some("shlib"),
        )
        .unwrap();
        assert!(one.record.argv.contains(&"-fPIC".to_string()));
        let none = pick(find(&records, "src/x.c", &roots()), "src/x.c", None);
        assert!(none.err().unwrap().starts_with("no compile of src/x.c"));
    }

    #[test]
    fn preprocessing_keeps_the_command_and_drops_the_outputs() {
        let (records, _) = parse_log(LOG);
        let rest = &records[0].argv[1..];
        let args = preprocess_args(rest, Path::new("/tmp/r/gram.i"));
        assert_eq!(
            args,
            [
                "-Isrc/backend/parser",
                "-I../../src/REL_18_6/src/include",
                "-D_GNU_SOURCE",
                "-O2",
                "-g",
                "-fwrapv",
                "src/backend/parser/gram.c",
                "-E",
                "-o",
                "/tmp/r/gram.i"
            ]
        );
    }

    #[test]
    fn recompiling_drops_the_preprocessor_options() {
        let (records, _) = parse_log(LOG);
        let args = compile_args(
            &records[1].argv[1..],
            "../../src/REL_18_6/src/port/path.c",
            "path.i",
            "path.o",
        );
        assert_eq!(args, ["-O2", "-c", "path.i", "-o", "path.o"]);
        let args = compile_args(
            &[
                "-D".into(),
                "X".into(),
                "-x".into(),
                "c".into(),
                "-Wp,-MD,x.d".into(),
                "-Wall".into(),
                "-pthread".into(),
                "a.c".into(),
            ],
            "a.c",
            "a.i",
            "a.o",
        );
        assert_eq!(args, ["-Wall", "-pthread", "-c", "a.i", "-o", "a.o"]);
    }

    #[test]
    fn the_bundle_texts_name_the_directory_and_quote_what_needs_it() {
        let (records, _) = parse_log(LOG);
        let text = command_text(&records[0], "src/backend/parser/gram.c");
        assert!(text.contains("directory: /w/b/build\n"));
        assert!(text.contains("status:    exited 0\n"));
        assert!(text.contains("env:       LANG=C.UTF-8\n"));
        assert!(text.contains("-frucc-trace=/tmp/t.jsonl (by the shim"));
        assert!(
            text.contains("\ncd /w/b/build\n/opt/rucc/target/release/rucc -Isrc/backend/parser")
        );
        let script = script_text(
            "/opt/rucc/target/release/rucc",
            &["-DX=a b".into(), "-c".into(), "gram.i".into()],
            "src/backend/parser/gram.c",
        );
        assert!(script.contains(
            "exec \"${CC:-/opt/rucc/target/release/rucc}\" '-DX=a b' -c gram.i \"$@\"\n"
        ));
        assert_eq!(
            default_out(Path::new("/w/b"), "src/backend/parser/gram.c"),
            Path::new("/w/b/repro/src-backend-parser-gram")
        );
    }
}
