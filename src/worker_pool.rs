use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

use crate::config::{self, WORKER_NUM_ENGINES};

/// A persistent translation worker subprocess.
///
/// The worker hosts N `EMTTranslator` engines (with `useGlobalTranslationQueue:NO`)
/// and stays alive across batches via a count-based stdin/stdout protocol.
///
/// Created automatically by [`LanguageTranslator::translate_batch`] — library
/// users don't need to interact with this type directly.
pub struct WorkerPool {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
}

impl WorkerPool {
    /// Try to create a worker pool for the given language pair.
    ///
    /// Auto-discovers the worker binary, assets directory, and locale mapping.
    /// Returns `None` if any prerequisite is missing (worker binary not found,
    /// assets not installed, etc.). Callers should fall back to the
    /// TranslationSession path.
    pub fn try_create(src: &str, tgt: &str) -> Option<Self> {
        let worker_bin = config::find_worker_bin()?;
        let assets_dir = config::find_assets_dir(src, tgt)?;
        let src_icu = config::normalize_locale(src);
        let tgt_icu = config::normalize_locale(tgt);

        Self::spawn(
            &worker_bin.to_string_lossy(),
            &assets_dir.to_string_lossy(),
            &src_icu,
            &tgt_icu,
            WORKER_NUM_ENGINES,
        )
        .ok()
    }

    /// Spawn a new worker subprocess with explicit parameters.
    fn spawn(
        worker_bin: &str,
        assets_dir: &str,
        src_lang: &str,
        tgt_lang: &str,
        num_engines: usize,
    ) -> Result<Self, String> {
        let mut child = Command::new(worker_bin)
            .arg(assets_dir)
            .arg(num_engines.to_string())
            .arg(src_lang)
            .arg(tgt_lang)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .map_err(|e| format!("Failed to spawn translation worker: {e}"))?;

        let stdin = child.stdin.take().ok_or("Failed to get worker stdin")?;
        let stdout = child.stdout.take().ok_or("Failed to get worker stdout")?;

        Ok(Self {
            child,
            stdin,
            stdout: BufReader::new(stdout),
        })
    }

    /// Translate a batch of texts. Returns one result per input, same order.
    pub fn translate_batch(&mut self, texts: &[String]) -> Vec<String> {
        if texts.is_empty() {
            return Vec::new();
        }

        // Write count, then texts.
        if let Err(e) = writeln!(self.stdin, "{}", texts.len()) {
            eprintln!("apple-translate-rs-sync: write count failed: {e}");
            return texts.iter().map(|_| String::new()).collect();
        }
        for text in texts {
            if let Err(e) = writeln!(self.stdin, "{text}") {
                eprintln!("apple-translate-rs-sync: write text failed: {e}");
                return texts.iter().map(|_| String::new()).collect();
            }
        }
        if let Err(e) = self.stdin.flush() {
            eprintln!("apple-translate-rs-sync: flush failed: {e}");
            return texts.iter().map(|_| String::new()).collect();
        }

        // Read results.
        let mut results = Vec::with_capacity(texts.len());
        let mut line = String::new();

        while results.len() < texts.len() {
            line.clear();
            match self.stdout.read_line(&mut line) {
                Ok(0) => break,
                Ok(_) => {
                    if line.ends_with('\n') {
                        line.pop();
                        if line.ends_with('\r') {
                            line.pop();
                        }
                    }
                    results.push(std::mem::take(&mut line));
                }
                Err(e) => {
                    eprintln!("apple-translate-rs-sync: read failed: {e}");
                    break;
                }
            }
        }

        results.resize(texts.len(), String::new());
        results
    }

    /// Shut down the worker subprocess.
    pub fn shutdown(&mut self) {
        // Send count=0 to signal exit.
        let _ = writeln!(self.stdin, "0");
        let _ = self.stdin.flush();
        let _ = self.child.wait();
    }
}

impl Drop for WorkerPool {
    fn drop(&mut self) {
        self.shutdown();
    }
}
