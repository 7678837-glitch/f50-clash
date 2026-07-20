use oxc::{
    allocator::Allocator,
    codegen::{Codegen, CodegenOptions},
    minifier::{MangleOptions, Minifier, MinifierOptions},
    parser::Parser,
    span::SourceType,
};
use std::{env, fs, path::PathBuf, process::Command};

const SOURCE: &str = "src/web/index.html";

fn main() {
    println!("cargo:rerun-if-changed={SOURCE}");
    println!("cargo:rerun-if-changed=VERSION");
    println!("cargo:rerun-if-env-changed=F50MGR_GIT_COMMIT");

    let version = release_version();
    let cargo_version = env::var("CARGO_PKG_VERSION").expect("CARGO_PKG_VERSION 未设置");
    assert_eq!(
        cargo_version,
        format!("{version}.0"),
        "Cargo.toml version 必须为 VERSION 后追加 .0"
    );
    println!("cargo:rustc-env=F50MGR_VERSION={version}");
    println!("cargo:rustc-env=F50MGR_GIT_COMMIT={}", git_commit());
    println!("cargo:rustc-env=F50MGR_SCHEMA_VERSION=1");

    let html = fs::read_to_string(SOURCE).expect("读取 Web 页面源码失败");
    let (head, rest) = html.split_once("<script>").expect("页面缺少 <script>");
    let (script, tail) = rest.split_once("</script>").expect("页面缺少 </script>");

    let allocator = Allocator::default();
    let parsed = Parser::new(&allocator, script, SourceType::script()).parse();
    assert!(
        parsed.diagnostics.is_empty(),
        "Web JavaScript 解析失败: {:?}",
        parsed.diagnostics
    );
    let mut program = parsed.program;
    let options = MinifierOptions {
        mangle: Some(MangleOptions {
            top_level: Some(true),
            ..MangleOptions::default()
        }),
        ..MinifierOptions::default()
    };
    let result = Minifier::new(options).minify(&allocator, &mut program);
    let minified = Codegen::new()
        .with_options(CodegenOptions::minify())
        .with_scoping(result.scoping)
        .build(&program)
        .code;

    assert!(
        minified.len() * 5 <= script.len() * 4,
        "Web JavaScript 压缩率未达到 20%: {} -> {} bytes",
        script.len(),
        minified.len()
    );

    let output = format!("{head}<script src=\"/assets/app.js\" defer></script>{tail}");
    let output_dir = PathBuf::from(env::var_os("OUT_DIR").expect("OUT_DIR 未设置"));
    fs::write(output_dir.join("index.html"), output).expect("写入 Web 页面失败");
    fs::write(output_dir.join("app.js"), minified).expect("写入压缩 Web JavaScript 失败");
}

fn release_version() -> String {
    let version = fs::read_to_string("VERSION")
        .expect("读取 VERSION 失败")
        .trim()
        .to_owned();
    let (date, sequence) = version.split_once('.').expect("VERSION 必须为 YYYYMMDD.N");
    assert!(
        date.len() == 8 && date.bytes().all(|b| b.is_ascii_digit()),
        "VERSION 日期必须为 8 位数字"
    );
    assert!(
        !sequence.is_empty()
            && sequence.bytes().all(|b| b.is_ascii_digit())
            && sequence.parse::<u32>().is_ok_and(|n| n > 0),
        "VERSION 序号必须为正整数"
    );
    version
}

fn git_commit() -> String {
    let commit = env::var("F50MGR_GIT_COMMIT").unwrap_or_else(|_| {
        Command::new("git")
            .args(["rev-parse", "--short=12", "HEAD"])
            .output()
            .ok()
            .filter(|output| output.status.success())
            .and_then(|output| String::from_utf8(output.stdout).ok())
            .map(|value| value.trim().to_owned())
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| "unknown".to_owned())
    });
    assert!(
        commit == "unknown" || commit.bytes().all(|b| b.is_ascii_hexdigit()),
        "F50MGR_GIT_COMMIT 必须为十六进制提交号"
    );
    commit
}
