use std::io::{BufRead, BufReader, Read, Write};
use std::process::{Child, ChildStderr, ChildStdin, ChildStdout, Command, Stdio};
use std::thread;

use crate::config;

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
    terminated: bool,
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
            config::worker_num_engines(),
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
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| format!("Failed to spawn translation worker: {e}"))?;

        let stdin = child.stdin.take().ok_or("Failed to get worker stdin")?;
        let stdout = child.stdout.take().ok_or("Failed to get worker stdout")?;
        let stderr = child.stderr.take().ok_or("Failed to get worker stderr")?;
        drain_worker_stderr(stderr);

        Ok(Self {
            child,
            stdin,
            stdout: BufReader::new(stdout),
            terminated: false,
        })
    }

    /// Translate a batch of texts. Returns one result per input, same order.
    pub fn translate_batch(&mut self, texts: &[String]) -> Result<Vec<String>, String> {
        if texts.is_empty() {
            return Ok(Vec::new());
        }

        // Write count, then length-prefixed UTF-8 texts.
        if let Err(e) = writeln!(self.stdin, "{}", texts.len()) {
            return Err(format!("write count failed: {e}"));
        }
        for text in texts {
            let bytes = text.as_bytes();
            if let Err(e) = writeln!(self.stdin, "{}", bytes.len()) {
                return Err(format!("write text length failed: {e}"));
            }
            if let Err(e) = self.stdin.write_all(bytes) {
                return Err(format!("write text bytes failed: {e}"));
            }
        }
        if let Err(e) = self.stdin.flush() {
            return Err(format!("flush failed: {e}"));
        }

        // Read length-prefixed UTF-8 results.
        let mut results = Vec::with_capacity(texts.len());
        let mut line = String::new();

        while results.len() < texts.len() {
            line.clear();
            match self.stdout.read_line(&mut line) {
                Ok(0) => return Err("worker closed stdout".to_owned()),
                Ok(_) => {
                    let len = match line.trim_end().parse::<usize>() {
                        Ok(len) => len,
                        Err(e) => {
                            return Err(format!(
                                "invalid result length {:?}: {e}",
                                line.trim_end()
                            ));
                        }
                    };
                    let mut bytes = vec![0; len];
                    if let Err(e) = self.stdout.read_exact(&mut bytes) {
                        return Err(format!("read result bytes failed: {e}"));
                    }
                    let result = String::from_utf8(bytes)
                        .map_err(|e| format!("worker returned non-UTF-8 result: {e}"))?;
                    results.push(result);
                }
                Err(e) => {
                    return Err(format!("read failed: {e}"));
                }
            }
        }

        Ok(results)
    }

    pub fn terminate(&mut self) {
        if self.terminated {
            return;
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
        self.terminated = true;
    }

    /// Shut down the worker subprocess.
    pub fn shutdown(&mut self) {
        if self.terminated {
            return;
        }
        // Send count=0 to signal exit.
        let _ = writeln!(self.stdin, "0");
        let _ = self.stdin.flush();
        let _ = self.child.wait();
        self.terminated = true;
    }
}

fn drain_worker_stderr(stderr: ChildStderr) {
    thread::spawn(move || {
        let reader = BufReader::new(stderr);
        for line in reader.lines() {
            if line.is_err() {
                break;
            }
        }
    });
}

impl Drop for WorkerPool {
    fn drop(&mut self) {
        self.shutdown();
    }
}
