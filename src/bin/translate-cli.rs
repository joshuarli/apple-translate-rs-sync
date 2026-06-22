use std::env;
use std::io::{self, IsTerminal, Read};
use std::process;

use apple_translate_rs_sync::{self as mt, LanguageTranslator, TranslationError};

fn usage() -> ! {
    eprintln!(
        "\
Usage: translate-cli [OPTIONS] [TEXT]

  Translate text using Apple's on-device Translation framework.
  The source language is auto-detected when --from is omitted.
  Text is read from stdin by default. Pipe text in or type/paste
  directly and press Ctrl+D to translate.

Options:
  -f, --from LANG    Source language (auto-detected if omitted)
  -t, --to LANG      Target language (default: en)
  -h, --help         Show this help

Examples:
  translate-cli 'Hola, mundo!'
  translate-cli --to fr 'Hello, world!'
  translate-cli --from en --to zh-Hans 'Good morning'
  translate-cli --to ja < spanish-text.txt
  echo 'Bonjour le monde' | translate-cli

Language codes use BCP-47 format: en, es, fr, de, ja, zh-Hans, zh-Hant, etc.
"
    );
    process::exit(1);
}

fn install_help() -> ! {
    eprintln!(
        "\
Models must be downloaded before translation can run:

  System Settings → General → Language & Region → Translation
  (or search 'Translation' in System Settings)

There is no programmatic download API in the headless Translation
framework — this is an Apple limitation.
"
    );
    process::exit(1);
}

fn parse_args() -> (Option<String>, String, Option<String>) {
    let mut from: Option<String> = None;
    let mut to = "en".to_owned();
    let mut text: Option<String> = None;
    let args: Vec<String> = env::args().skip(1).collect();
    let mut i = 0;

    while i < args.len() {
        match args[i].as_str() {
            "-f" | "--from" => {
                i += 1;
                if i >= args.len() {
                    eprintln!("error: --from requires a language code");
                    process::exit(1);
                }
                from = Some(args[i].clone());
            }
            "-t" | "--to" => {
                i += 1;
                if i >= args.len() {
                    eprintln!("error: --to requires a language code");
                    process::exit(1);
                }
                to = args[i].clone();
            }
            "--install" | "--download" => install_help(),
            "-h" | "--help" => usage(),
            arg if arg.starts_with('-') => {
                eprintln!("error: unknown flag: {arg}");
                process::exit(1);
            }
            _ => {
                let t = text.get_or_insert(String::new());
                if !t.is_empty() {
                    t.push(' ');
                }
                t.push_str(&args[i]);
            }
        }
        i += 1;
    }

    (from, to, text)
}

fn main() {
    let (from_opt, to, text_opt) = parse_args();

    let text = match text_opt {
        Some(t) => t,
        None => {
            if io::stdin().is_terminal() {
                eprintln!(
                    "Reading from stdin. Type or paste text, then press Ctrl+D to translate."
                );
            }
            let mut stdin_text = String::new();
            io::stdin()
                .read_to_string(&mut stdin_text)
                .unwrap_or_else(|e| {
                    eprintln!("error: reading stdin: {e}");
                    process::exit(1);
                });
            let t = stdin_text.trim_end().to_owned();
            if t.is_empty() {
                usage();
            }
            t
        }
    };

    let from = match from_opt {
        Some(lang) => lang,
        None => match mt::detect_language(&text) {
            Some(lang) => {
                eprintln!("[detected source: {lang}]");
                lang
            }
            None => {
                eprintln!("error: could not auto-detect source language");
                eprintln!("  Hint: specify the source language with --from LANG");
                process::exit(1);
            }
        },
    };

    let translator = match LanguageTranslator::new(&from, &to) {
        Ok(t) => t,
        Err(TranslationError::LanguageNotInstalled { .. })
        | Err(TranslationError::LanguageUnsupported { .. }) => {
            eprintln!("error: language pair not available: {from} -> {to}");
            install_help();
        }
        Err(e) => {
            eprintln!("error: {e}");
            process::exit(1);
        }
    };

    match translator.translate(&text) {
        Ok(response) => println!("{}", response.target_text),
        Err(e) => {
            eprintln!("error: {e}");
            process::exit(1);
        }
    }
}
