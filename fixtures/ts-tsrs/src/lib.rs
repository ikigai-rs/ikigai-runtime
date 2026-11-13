//! tsrs as the spike's TypeScript step (ledger #3659): one compile over an in-memory file system,
//! the same code natively (`src/main.rs`) and inside ikigai-runtime (`session`, wasm32 only).
//!
//! Every source is handed in: the file system is tsrs's own `MapFS`, holding exactly the files
//! given, under tsrs's embedded `lib.*.d.ts`. Nothing is read from the host's disk, and what tsc
//! writes stays in the map.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tsrs_ast::Diagnostic;
use tsrs_core::P;
use tsrs_execute::tsc::{ReadWriteCloser, System};
use tsrs_vfs::{FS, bundled, vfstest};

/// The directory the sources are placed in.
pub const ROOT: &str = "/src";

/// Emit settings: types stripped, nothing checked or lowered, as esbuild's `--format=esm`. The
/// smallest library and no imports followed: an unchecked emit needs neither, and the default
/// `esnext` libraries (DOM included) are most of its time otherwise. `--noLib` is faster still,
/// but tsc then reports the missing global types even with `--noCheck`.
pub const EMIT: &[&str] = &[
    "--target",
    "esnext",
    "--module",
    "esnext",
    "--noCheck",
    "--lib",
    "es5",
    "--types",
    "",
    "--noResolve",
    "--isolatedModules",
    "--removeComments",
    "--outDir",
    "/out",
];

/// Check settings: strict, no output, the libraries an ES2023 engine with no DOM has.
pub const CHECK: &[&str] = &[
    "--noEmit", "--strict", "--target", "es2023", "--lib", "es2023", "--types", "",
];

/// One diagnostic, located in the file it names (1-based line and column).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Problem {
    pub file: String,
    pub line: u32,
    pub column: u32,
    pub code: i32,
    pub message: String,
}

impl std::fmt::Display for Problem {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{}:{}:{}: TS{}: {}",
            self.file, self.line, self.column, self.code, self.message
        )
    }
}

/// What a compile did: tsc's exit status, the files it wrote, its diagnostics and its time.
#[derive(Debug)]
pub struct Outcome {
    pub status: i32,
    pub written: Vec<(String, String)>,
    pub problems: Vec<Problem>,
    pub elapsed: Duration,
}

struct MemSys {
    fs: Arc<dyn FS>,
    lib: String,
    start: Instant,
    sink: Box<dyn Fn(P<Diagnostic>) + Send + Sync>,
}

impl System for MemSys {
    fn fs(&self) -> Arc<dyn FS> {
        Arc::clone(&self.fs)
    }
    fn default_library_path(&self) -> &str {
        &self.lib
    }
    fn get_current_directory(&self) -> &str {
        ROOT
    }
    fn write(&self, _: &str) {}
    fn flush(&self) {}
    fn write_output_is_tty(&self) -> bool {
        false
    }
    fn get_environment_variable(&self, _: &str) -> Option<String> {
        None
    }
    fn spawn(
        &self,
        _: &[String],
        _: &str,
        _: Option<Box<dyn std::io::Write + Send>>,
    ) -> Result<ReadWriteCloser, String> {
        Err("no processes here".into())
    }
    fn now(&self) -> Instant {
        Instant::now()
    }
    fn since_start(&self) -> Duration {
        self.start.elapsed()
    }
    fn diagnostic_sink(&self) -> Option<&(dyn Fn(P<Diagnostic>) + Sync)> {
        Some(&*self.sink)
    }
}

fn locate(d: &Diagnostic) -> Problem {
    let (file, line, column) = match d.file() {
        Some(file) => {
            let text = file.text();
            let pos = (d.pos().unwrap_or(0) as usize).min(text.len());
            let before = &text[..pos];
            let line = before.matches('\n').count() + 1;
            let column = before.rsplit('\n').next().unwrap_or("").chars().count() + 1;
            (file.file_name().to_string(), line as u32, column as u32)
        }
        None => (String::new(), 0, 0),
    };
    let mut message = d.localize();
    for chained in d.message_chain() {
        message.push(' ');
        message.push_str(&chained.localize());
    }
    Problem {
        file,
        line,
        column,
        code: d.code(),
        message,
    }
}

/// Compile `files` (names relative to [`ROOT`]) with tsc's `args`; the root files are the `.ts`
/// files given that are not declarations, plus every declaration.
pub fn compile(files: &[(&str, &str)], args: &[&str]) -> Outcome {
    let started = Instant::now();
    // The one-thread pool must exist before any other rayon use (as tsrs_wasm does).
    tsrs_compiler::worker_pool();
    let fs = vfstest::from_map(
        files
            .iter()
            .map(|(name, text)| (format!("{ROOT}/{name}"), text.to_string())),
        true,
    );
    let fs: Arc<dyn FS> = Arc::new(bundled::wrap_fs(fs));
    let problems = Arc::new(Mutex::new(Vec::new()));
    let seen = Arc::clone(&problems);
    // tsc's driver takes a `'static` system; one per compile.
    let sys: &'static MemSys = Box::leak(Box::new(MemSys {
        fs: Arc::clone(&fs),
        lib: bundled::lib_path(),
        start: started,
        sink: Box::new(move |d| seen.lock().unwrap().push(locate(&d))),
    }));
    let mut command: Vec<String> = args.iter().map(|a| a.to_string()).collect();
    command.extend(files.iter().map(|(name, _)| name.to_string()));
    let result = tsrs_execute::execute::command_line(sys, command);
    let written = files
        .iter()
        .filter(|(name, _)| !name.ends_with(".d.ts"))
        .filter_map(|(name, _)| {
            let js = format!("/out/{}.js", name.trim_end_matches(".ts"));
            fs.read_file(&js).map(|text| (js, text))
        })
        .collect();
    let problems = problems.lock().unwrap().clone();
    Outcome {
        status: result.status as i32,
        written,
        problems,
        elapsed: started.elapsed(),
    }
}

#[cfg(target_family = "wasm")]
mod session;
