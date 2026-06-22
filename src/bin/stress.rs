use std::env;
use std::io::{self, BufRead, Read, Write};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use apple_translate_rs_sync::{
    LanguageTranslator, TranslationRequest, check_language_availability, detect_language,
};

const PAIR: (&str, &str) = ("zh-Hans", "en");
const SMOKE_TEXT: &str = "你好";
const FUNCTIONAL_TEXTS: &[&str] = &["你好", "世界", "谢谢"];
const SHORT_TEXTS: &[&str] = &[
    "你好，今天天气怎么样？",
    "我明天要去超市买东西。",
    "这本书非常有趣，我推荐你也读一读。",
    "请问最近的火车站在哪里？",
    "今天的会议推迟到下午三点。",
    "这是我的朋友，他在一家科技公司工作。",
    "我想学习一门新的语言，你有什么建议吗？",
    "昨天我去公园散步，看到了很多花。",
    "这个问题的答案不是很明显。",
    "我们需要更多的时间来完成这个项目。",
];
const LONG_TEXT: &str = "气候变化是当今世界面临的最严峻挑战之一。根据政府间气候变化专门委员会（IPCC）的最新报告，全球平均气温已经比工业化前水平上升了约1.1摄氏度。这一变化虽然看似微小，但已经对地球生态系统产生了深远影响。极端天气事件的频率和强度不断增加，从澳大利亚的丛林大火到欧洲的洪水，从北极海冰的融化到太平洋岛国面临的海平面上升威胁。科学家们警告说，如果不采取果断行动减少温室气体排放，到本世纪末全球气温可能上升3摄氏度以上，这将带来灾难性的后果。人工智能技术的快速发展正在深刻改变我们的生活方式和工作模式。从智能手机中的语音助手到自动驾驶汽车，从医疗诊断中的图像识别到金融领域的风险评估，人工智能的应用已经渗透到社会的方方面面。深度学习算法的突破使得计算机能够处理和分析海量数据，从中提取有价值的模式和见解。然而，人工智能的广泛应用也引发了诸多伦理和社会问题，包括隐私保护、就业替代、算法偏见以及自主武器系统等。各国政府和研究机构正在积极探索人工智能治理框架，以确保这项技术的发展能够造福全人类。";

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    All,
    Functional,
    Batch,
    Long,
    Parallel,
    WorkerBatch,
    WorkerLong,
}

struct State {
    passed: usize,
    failed: usize,
}

#[derive(Clone, Copy)]
struct Throughput {
    ok: usize,
    fail: usize,
    elapsed: Duration,
}

fn main() {
    let mode = parse_mode();
    match mode {
        Mode::WorkerBatch => run_worker_batch(),
        Mode::WorkerLong => run_worker_long(),
        Mode::Functional => exit_with(run_functional()),
        Mode::Batch => exit_with(run_batch_stress()),
        Mode::Long => exit_with(run_long_stress()),
        Mode::Parallel => exit_with(run_parallel_stress()),
        Mode::All => {
            let mut failed = 0;
            failed += run_functional();
            failed += run_batch_stress();
            failed += run_long_stress();
            failed += run_parallel_stress();
            exit_with(failed);
        }
    }
}

fn parse_mode() -> Mode {
    let mut args = env::args().skip(1);
    let Some(mode) = args.next() else {
        return Mode::All;
    };

    match mode.as_str() {
        "all" => Mode::All,
        "functional" | "harness" | "smoke" => Mode::Functional,
        "batch" | "short" => Mode::Batch,
        "long" => Mode::Long,
        "parallel" => Mode::Parallel,
        "worker" => match args.next().as_deref() {
            Some("long") => Mode::WorkerLong,
            _ => Mode::WorkerBatch,
        },
        "-h" | "--help" | "help" => usage(),
        other => {
            eprintln!("unknown stress mode: {other}");
            usage();
        }
    }
}

fn usage() -> ! {
    eprintln!(
        "\
Usage: stress [MODE]

Modes:
  all          Run functional checks plus all throughput checks (default)
  functional   End-to-end API smoke checks
  batch        Short-text translate_batch throughput
  long         Article-length translate_batch throughput
  parallel     Concurrent translate() throughput for TranslationSession fallback

Internal:
  worker [batch|long]
"
    );
    std::process::exit(2);
}

fn exit_with(failed: usize) -> ! {
    if failed > 0 {
        std::process::exit(1);
    }
    std::process::exit(0);
}

fn translator() -> Result<LanguageTranslator, String> {
    LanguageTranslator::new(PAIR.0, PAIR.1).map_err(|e| e.to_string())
}

fn run_functional() -> usize {
    let mut s = State {
        passed: 0,
        failed: 0,
    };

    eprintln!("=== Functional ({}, {}) ===", PAIR.0, PAIR.1);

    match detect_language("Hello, world!") {
        Some(ref lang) if lang == "en" => s.pass("detect English"),
        other => s.fail(
            "detect English",
            format_args!("expected Some(\"en\"), got {other:?}"),
        ),
    }
    match detect_language("你好世界") {
        Some(ref lang) if lang.starts_with("zh") => s.pass("detect Chinese"),
        other => s.fail(
            "detect Chinese",
            format_args!("expected zh-*, got {other:?}"),
        ),
    }
    if detect_language("").is_none() {
        s.pass("detect empty");
    } else {
        s.fail("detect empty", "expected None");
    }

    match check_language_availability(PAIR.0, PAIR.1) {
        Ok(()) => s.pass("availability"),
        Err(e) => s.fail("availability", e),
    }
    match check_language_availability("en", "zz") {
        Err(_) => s.pass("invalid pair errors"),
        Ok(()) => s.fail("invalid pair errors", "expected error for en -> zz"),
    }

    let t = match translator() {
        Ok(t) => t,
        Err(e) => {
            s.fail("translator", e);
            return s.finish();
        }
    };

    if t.source() == PAIR.0 && t.target() == PAIR.1 {
        s.pass("source/target accessors");
    } else {
        s.fail("source/target accessors", "unexpected language pair");
    }

    let t2 = t.clone();
    if t2.source() == t.source() && t2.target() == t.target() {
        s.pass("clone");
    } else {
        s.fail("clone", "mismatch");
    }

    match t.translate(SMOKE_TEXT) {
        Ok(r)
            if !r.target_text.is_empty()
                && r.source_text == SMOKE_TEXT
                && r.source_language == PAIR.0
                && r.target_language == PAIR.1 =>
        {
            eprintln!("  '{}' -> '{}'", SMOKE_TEXT, r.target_text);
            s.pass("translate single");
        }
        Ok(r) => s.fail(
            "translate single",
            format_args!("unexpected response: {r:?}"),
        ),
        Err(e) => s.fail("translate single", e),
    }

    let requests: Vec<TranslationRequest> = FUNCTIONAL_TEXTS
        .iter()
        .map(|t| TranslationRequest::new(*t))
        .collect();
    let results = t.translate_batch(&requests);
    if results.len() != requests.len() {
        s.fail(
            "batch length",
            format_args!("expected {}, got {}", requests.len(), results.len()),
        );
    } else {
        let mut ok = true;
        for (req, result) in requests.iter().zip(&results) {
            match result {
                Ok(r) if r.source_text == req.source_text && !r.target_text.is_empty() => {
                    eprintln!("  '{}' -> '{}'", req.source_text, r.target_text);
                }
                Ok(r) => {
                    ok = false;
                    s.fail("batch item", format_args!("unexpected response: {r:?}"));
                }
                Err(e) => {
                    ok = false;
                    s.fail("batch item", e);
                }
            }
        }
        if ok {
            s.pass("batch");
        }
    }

    let reqs_with_ids = vec![
        TranslationRequest::with_client_id("你好", "a"),
        TranslationRequest::with_client_id("世界", "b"),
    ];
    let ids_ok = t
        .translate_batch(&reqs_with_ids)
        .iter()
        .zip(&reqs_with_ids)
        .all(|(result, req)| {
            result
                .as_ref()
                .is_ok_and(|r| r.client_identifier == req.client_identifier)
        });
    if ids_ok {
        s.pass("batch client IDs");
    } else {
        s.fail("batch client IDs", "mismatch");
    }

    if t.translate_batch(&[]).is_empty() {
        s.pass("batch empty");
    } else {
        s.fail("batch empty", "expected empty");
    }

    match t.prepare() {
        Ok(()) => s.pass("prepare"),
        Err(e) => s.fail("prepare", e),
    }

    let req: TranslationRequest = "hello".into();
    if req.source_text == "hello" && req.client_identifier.is_none() {
        s.pass("From<&str>");
    } else {
        s.fail("From<&str>", "unexpected value");
    }

    let req: TranslationRequest = String::from("hello").into();
    if req.source_text == "hello" {
        s.pass("From<String>");
    } else {
        s.fail("From<String>", "unexpected value");
    }

    let req = TranslationRequest::with_client_id("hello", "id-1");
    if req.source_text == "hello" && req.client_identifier.as_deref() == Some("id-1") {
        s.pass("with_client_id");
    } else {
        s.fail("with_client_id", "unexpected value");
    }

    s.finish()
}

fn run_batch_stress() -> usize {
    eprintln!("\n=== Batch Stress ({}, {}) ===", PAIR.0, PAIR.1);
    let t = match translator() {
        Ok(t) => t,
        Err(e) => {
            eprintln!("FATAL: cannot create translator: {e}");
            return 1;
        }
    };

    let total_texts = 40;
    let pool: Vec<&str> = SHORT_TEXTS
        .iter()
        .cycle()
        .take(total_texts)
        .copied()
        .collect();
    let requests = requests_from_strs(&pool);

    eprintln!("single call, {total_texts} texts");
    let single = time_batch(&t, &requests);
    print_req_rate(single);

    let mut failed = usize::from(single.fail > 0);

    for n_threads in [2usize, 4, 8, 16] {
        eprintln!("{n_threads} threads, shared process");
        let result = threaded_batch(&t, &pool, n_threads);
        print_req_rate(result);
        failed += usize::from(result.fail > 0);
    }

    failed
}

fn run_long_stress() -> usize {
    eprintln!("\n=== Long Stress ({}, {}) ===", PAIR.0, PAIR.1);
    let text_len = LONG_TEXT.len();
    let result = process_long(1);
    print_char_rate(result, text_len);

    let mut failed = usize::from(result.fail > 0);
    for n_procs in [2usize, 4, 8, 16] {
        let result = process_long(n_procs);
        print_char_rate(result, text_len);
        failed += usize::from(result.fail > 0);
    }
    failed
}

fn run_parallel_stress() -> usize {
    eprintln!(
        "\n=== Parallel Translate Stress ({}, {}) ===",
        PAIR.0, PAIR.1
    );
    let t = match translator() {
        Ok(t) => t,
        Err(e) => {
            eprintln!("FATAL: cannot create translator: {e}");
            return 1;
        }
    };

    eprintln!("warming up translate()");
    if let Err(e) = t.translate(SMOKE_TEXT) {
        eprintln!("warmup failed: {e}");
        return 1;
    }

    let iterations = 20usize;
    let text_len = LONG_TEXT.len();

    eprintln!("single thread, {iterations} iterations");
    let start = Instant::now();
    let mut ok = 0;
    for _ in 0..iterations {
        if t.translate(LONG_TEXT).is_ok() {
            ok += 1;
        }
    }
    let elapsed = start.elapsed();
    let result = Throughput {
        ok,
        fail: iterations - ok,
        elapsed,
    };
    print_char_rate(result, text_len);
    let mut failed = usize::from(result.fail > 0);

    for n_threads in [2usize, 4, 8, 16] {
        eprintln!("{n_threads} threads, {iterations} iterations each");
        let start = Instant::now();
        let mut handles = Vec::with_capacity(n_threads);
        for _ in 0..n_threads {
            let t = t.clone();
            handles.push(thread::spawn(move || {
                let mut ok = 0;
                for _ in 0..iterations {
                    if t.translate(LONG_TEXT).is_ok() {
                        ok += 1;
                    }
                }
                ok
            }));
        }

        let ok: usize = handles.into_iter().map(|h| h.join().unwrap()).sum();
        let expected = n_threads * iterations;
        let result = Throughput {
            ok,
            fail: expected - ok,
            elapsed: start.elapsed(),
        };
        print_char_rate(result, text_len);
        failed += usize::from(result.fail > 0);
    }

    failed
}

fn requests_from_strs(texts: &[&str]) -> Vec<TranslationRequest> {
    texts
        .iter()
        .map(|text| TranslationRequest::new(*text))
        .collect()
}

fn requests_from_strings(texts: &[String]) -> Vec<TranslationRequest> {
    texts
        .iter()
        .map(|text| TranslationRequest::new(text.clone()))
        .collect()
}

fn time_batch(t: &LanguageTranslator, requests: &[TranslationRequest]) -> Throughput {
    let start = Instant::now();
    let results = t.translate_batch(requests);
    let elapsed = start.elapsed();
    let ok = results.iter().filter(|r| r.is_ok()).count();
    Throughput {
        ok,
        fail: results.len() - ok,
        elapsed,
    }
}

fn threaded_batch(t: &LanguageTranslator, pool: &[&str], n_threads: usize) -> Throughput {
    let base = pool.len() / n_threads;
    let rem = pool.len() % n_threads;
    let start = Instant::now();
    let mut handles = Vec::with_capacity(n_threads);
    let mut offset = 0;

    for i in 0..n_threads {
        let t = t.clone();
        let size = base + usize::from(i < rem);
        let chunk = requests_from_strs(&pool[offset..offset + size]);
        offset += size;
        handles.push(thread::spawn(move || {
            let results = t.translate_batch(&chunk);
            let ok = results.iter().filter(|r| r.is_ok()).count();
            (ok, results.len() - ok)
        }));
    }

    let mut ok = 0;
    let mut fail = 0;
    for h in handles {
        let (o, f) = h.join().unwrap();
        ok += o;
        fail += f;
    }

    Throughput {
        ok,
        fail,
        elapsed: start.elapsed(),
    }
}

fn process_long(n_procs: usize) -> Throughput {
    let exe = env::current_exe().expect("cannot get exe path");
    let start = Instant::now();
    let mut children = Vec::with_capacity(n_procs);

    for _ in 0..n_procs {
        let mut child = Command::new(&exe)
            .arg("worker")
            .arg("long")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("failed to spawn worker");

        {
            let mut stdin = child.stdin.take().unwrap();
            write!(stdin, "{LONG_TEXT}").expect("write worker stdin");
        }

        children.push(child);
    }

    let mut ok = 0;
    for mut child in children {
        let mut output = String::new();
        child
            .stdout
            .take()
            .unwrap()
            .read_to_string(&mut output)
            .expect("read worker stdout");
        if !output.is_empty() {
            ok += 1;
        }
        child.wait().expect("worker failed");
    }

    Throughput {
        ok,
        fail: n_procs - ok,
        elapsed: start.elapsed(),
    }
}

fn run_worker_batch() {
    let t = match translator() {
        Ok(t) => t,
        Err(e) => {
            eprintln!("worker: cannot create translator: {e}");
            std::process::exit(1);
        }
    };

    let stdin = io::stdin();
    let texts: Vec<String> = stdin.lock().lines().map(|l| l.unwrap()).collect();
    if texts.is_empty() {
        return;
    }

    let results = t.translate_batch(&requests_from_strings(&texts));
    let stdout = io::stdout();
    let mut handle = stdout.lock();
    for r in results.iter().flatten() {
        let _ = writeln!(handle, "{}", r.target_text);
    }
}

fn run_worker_long() {
    let t = match translator() {
        Ok(t) => t,
        Err(e) => {
            eprintln!("worker: cannot create translator: {e}");
            std::process::exit(1);
        }
    };

    let mut text = String::new();
    io::stdin().lock().read_to_string(&mut text).unwrap();
    if text.is_empty() {
        return;
    }

    let results = t.translate_batch(&[TranslationRequest::new(text)]);
    if let Some(Ok(r)) = results.into_iter().next() {
        println!("{}", r.target_text);
    }
}

fn print_req_rate(result: Throughput) {
    eprintln!(
        "  {} ok, {} fail in {:.2?} ({:.1} req/s)",
        result.ok,
        result.fail,
        result.elapsed,
        result.ok as f64 / result.elapsed.as_secs_f64()
    );
}

fn print_char_rate(result: Throughput, chars_per_item: usize) {
    let total_chars = result.ok * chars_per_item;
    eprintln!(
        "  {} ok, {} fail in {:.2?}, {} chars ({:.0} chars/s)",
        result.ok,
        result.fail,
        result.elapsed,
        total_chars,
        total_chars as f64 / result.elapsed.as_secs_f64()
    );
}

impl State {
    fn pass(&mut self, desc: &str) {
        self.passed += 1;
        eprintln!("  PASS  {desc}");
    }

    fn fail(&mut self, desc: &str, err: impl std::fmt::Display) {
        self.failed += 1;
        eprintln!("  FAIL  {desc}: {err}");
    }

    fn finish(self) -> usize {
        eprintln!(
            "functional result: {} passed, {} failed",
            self.passed, self.failed
        );
        self.failed
    }
}
