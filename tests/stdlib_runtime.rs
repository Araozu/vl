//! Execute stdlib behavior on an external, read-only Naravm host.
//! NARAVM_BIN=/path/to/naravm cargo test --test stdlib_runtime -- --ignored

use std::fs;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

struct TempDir(PathBuf);

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn run_vl(body: &str) {
    let vm = std::env::var_os("NARAVM_BIN").expect("set NARAVM_BIN to the Naravm executable");
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock is after the Unix epoch")
        .as_nanos();
    let dir = TempDir(
        std::env::temp_dir().join(format!("vl-stdlib-runtime-{}-{nonce}", std::process::id())),
    );
    fs::create_dir_all(&dir.0).expect("create test directory");
    let source = dir.0.join("probe.vl");
    let artifact = dir.0.join("probe.naravm");
    fs::write(
        &source,
        format!(
            "use std;\n{body}\nfun expect(ok: bool) {{ if (!ok) {{ std.println(\"FAIL\"); }} }}\n"
        ),
    )
    .expect("write runtime test");
    let compilation = Command::new(env!("CARGO_BIN_EXE_vl"))
        .args(["build", "--format", "json", "--out"])
        .arg(&artifact)
        .arg(&source)
        .output()
        .expect("run compiler");
    assert!(
        compilation.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&compilation.stdout),
        String::from_utf8_lossy(&compilation.stderr)
    );
    let mut child = Command::new(vm)
        .arg(&artifact)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("run Naravm");
    let started = Instant::now();
    loop {
        if child.try_wait().expect("poll Naravm").is_some() {
            break;
        }
        if started.elapsed() > Duration::from_secs(30) {
            child.kill().expect("stop stalled Naravm");
            let _ = child.wait();
            panic!("stdlib runtime test exceeded 30 seconds");
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    let output = child.wait_with_output().expect("collect Naravm output");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&output.stdout), "ok\n");
}

#[test]
#[ignore = "requires NARAVM_BIN pointing to an external Naravm host"]
fn ascii_and_checked_parsing_handle_boundaries_and_errors() {
    run_vl(
        r#"
use std.ascii;
use std.parse.{self, ParseError};
fun main() {
    expect(ascii.is_ascii("a\0"));
    expect(!ascii.is_ascii("é"));
    expect(ascii.is_lower(97u8));
    expect(ascii.is_upper(90u8));
    expect(ascii.is_alnum(48u8));
    expect(ascii.is_control(127u8));
    expect(!ascii.is_control(128u8));
    expect(ascii.is_printable(32u8));
    expect(!ascii.is_printable(127u8));
    expect(ascii.is_punctuation(33u8));
    expect(!ascii.is_punctuation(65u8));
    expect(ascii.is_alpha(65u8));
    expect(!ascii.is_alpha(64u8));
    expect(!ascii.is_alpha(128u8));
    expect(ascii.is_digit(48u8));
    expect(!ascii.is_digit(47u8));
    expect(ascii.is_hex_digit(102u8));
    expect(!ascii.is_hex_digit(103u8));
    expect(ascii.is_whitespace(11u8));
    expect(!ascii.is_whitespace(0u8));
    expect(ascii.to_lower(90u8) == 122u8);
    expect(ascii.to_upper(97u8) == 65u8);
    expect(ascii.to_upper(255u8) == 255u8);
    expect((parse.u64("18446744073709551615") catch 0u64) == 18446744073709551615u64);
    expect((parse.u64("00042") catch 0u64) == 42u64);
    expect((parse.hex_u64("ffffffffffffffff") catch 0u64) == 18446744073709551615u64);
    expect((parse.hex_u64("AbC") catch 0u64) == 2748u64);
    expect(parse.bool("true") catch false);
    expect(!(parse.bool("false") catch true));
    val overflow = parse.u64("18446744073709551616");
    match (overflow) {
        ParseError.Overflow { expect(true); }
        else { expect(false); }
    }
    val hex_overflow = parse.hex_u64("10000000000000000");
    match (hex_overflow) {
        ParseError.Overflow { expect(true); }
        else { expect(false); }
    }
    val empty = parse.u64("");
    match (empty) {
        ParseError.Empty { expect(true); }
        else { expect(false); }
    }
    val invalid = parse.u64(" 1");
    match (invalid) {
        ParseError.InvalidDigit { expect(true); }
        else { expect(false); }
    }
    val invalid_hex = parse.hex_u64("0x10");
    match (invalid_hex) {
        ParseError.InvalidDigit { expect(true); }
        else { expect(false); }
    }
    val invalid_bool = parse.bool("TRUE");
    match (invalid_bool) {
        ParseError.InvalidDigit { expect(true); }
        else { expect(false); }
    }
    std.println("ok");
}

"#,
    );
}

#[test]
#[ignore = "requires NARAVM_BIN pointing to an external Naravm host"]
fn signed_parsing_and_formatting_cover_integer_extremes() {
    run_vl(
        r#"
use std.parse.{self, ParseError};
use std.fmt;
fun main() {
    expect((parse.i64("9223372036854775807") catch 0i64) == 9223372036854775807i64);
    expect((parse.i64("-9223372036854775808") catch 0i64) == (-9223372036854775807i64 - 1i64));
    expect((parse.i64("-0") catch 1i64) == 0i64);
    val positive_overflow = parse.i64("9223372036854775808");
    match (positive_overflow) { ParseError.Overflow { expect(true); } else { expect(false); } }
    val negative_overflow = parse.i64("-9223372036854775809");
    match (negative_overflow) { ParseError.Overflow { expect(true); } else { expect(false); } }
    val plus = parse.i64("+1");
    match (plus) { ParseError.InvalidSign { expect(true); } else { expect(false); } }
    val sign_only = parse.i64("-");
    match (sign_only) { ParseError.Empty { expect(true); } else { expect(false); } }
    expect(fmt.i64_to_string(-9223372036854775807i64 - 1i64) == "-9223372036854775808");
    expect(fmt.i64_to_string(9223372036854775807i64) == "9223372036854775807");
    expect(fmt.i64_to_string(0i64) == "0");
    expect(fmt.bool_to_string(true) == "true");
    expect(fmt.bool_to_string(false) == "false");
    expect(fmt.hex_u64_to_string(0u64) == "0x0");
    expect(fmt.hex_u64_to_string(18446744073709551615u64) == "0xffffffffffffffff");
    expect(fmt.pad_right("x", 4u64, "ab") == "xaba");
    expect(fmt.pad_right("x", 4u64, "") == "x");
    expect(fmt.pad_left("x", 4u64, "ab") == "abax");
    expect(fmt.pad_left("long", 2u64, "0") == "long");
    expect(fmt.pad_left("x", 4u64, "") == "x");
    std.println("ok");
}
"#,
    );
}

#[test]
#[ignore = "requires NARAVM_BIN pointing to an external Naravm host"]
fn string_helpers_preserve_bytes_and_empty_fields() {
    run_vl(
        r#"
use std.string;
fun main() {
    expect(string.starts_with("abc", ""));
    expect(string.ends_with("abc", ""));
    expect(!string.starts_with("a", "abc"));
    expect(string.starts_with("abc", "ab"));
    expect(string.ends_with("abc", "bc"));
    expect(string.find("banana", "ana") == 1u64);
    expect(string.find_from("banana", "ana", 2u64) == 3u64);
    expect(string.find_from("banana", "", 6u64) == 6u64);
    expect(string.find_from("banana", "a", 18446744073709551615u64) == 6u64);
    expect(string.find("abc", "z") == 3u64);
    expect(string.contains("", ""));
    expect(!string.contains("", "x"));
    expect(string.trim(" \t\r\nhello \t") == "hello");
    expect(string.trim(" \t\n") == "");
    expect(string.trim("\0") == "\0");
    expect(string.trim_start(" \tvalue \n") == "value \n");
    expect(string.trim_end(" \tvalue \n") == " \tvalue");
    expect(string.trim_start(" \t\r\n") == "");
    expect(string.trim_end(" \t\r\n") == "");
    expect(string.repeat("x", 0u64) == "");
    expect(string.len(string.repeat("ab", 2000u64)) == 4000u64);
    expect(string.repeat("", 18446744073709551615u64) == "");
    expect(string.replace("aaaaa", "aa", "b") == "bba");
    expect(string.replace("abc", "", "x") == "abc");
    expect(string.replace("abc", "b", "") == "ac");
    val parts = string.split(",a,,b,", ",");
    expect(parts.len == 5u64);
    expect(parts[0] == "");
    expect(parts[1] == "a");
    expect(parts[2] == "");
    expect(parts[3] == "b");
    expect(parts[4] == "");
    expect(string.join(parts, ",") == ",a,,b,");
    val empty_parts = string.split("", ",");
    expect(empty_parts.len == 1u64);
    expect(empty_parts[0] == "");
    val unsplit = string.split("abc", "");
    expect(unsplit.len == 1u64);
    expect(unsplit[0] == "abc");
    val no_parts: Array[String] = [];
    expect(string.join(no_parts, ",") == "");
    val bytes = string.to_bytes("a\0é");
    expect(bytes.len == 4u64);
    expect(bytes[0] == 97u8);
    expect(bytes[1] == 0u8);
    expect(bytes[2] == 195u8);
    expect(bytes[3] == 169u8);
    std.println("ok");
}
"#,
    );
}

#[test]
#[ignore = "requires NARAVM_BIN pointing to an external Naravm host"]
fn array_helpers_preserve_copy_and_alias_semantics() {
    run_vl(
        r#"
use std.array.{self, ArrayError};
type Counter = object { value: u64, };
fun main() {
    var values: *Array[u64] = [3, 1, 2];
    val cloned: *Array[u64] = array.clone(values);
    array.fill(values, 9u64);
    expect(cloned[0] == 3u64);
    array.reverse(cloned);
    expect(cloned[0] == 2u64);
    expect(cloned[2] == 3u64);
    array.sort(cloned);
    expect(cloned[0] == 1u64);
    expect(cloned[1] == 2u64);
    expect(cloned[2] == 3u64);
    expect(array.sum_u64(cloned) == 6u64);
    expect(array.find(cloned, 2u64) == 1u64);
    expect(array.find(cloned, 7u64) == cloned.len);
    expect(array.contains(cloned, 3u64));
    expect(array.equal(cloned, [1u64, 2u64, 3u64]));
    expect(!array.equal(cloned, [1u64, 2u64]));
    var destination: *Array[u64] = [0, 0, 0, 0];
    expect(array.copy(cloned, destination, 1u64));
    expect(destination[0] == 0u64 && destination[3] == 3u64);
    expect(!array.copy(cloned, destination, 2u64));
    expect(destination[1] == 1u64);
    expect(!array.copy(cloned, destination, 18446744073709551615u64));
    val empty: Array[u64] = [];
    expect(array.is_empty(empty));
    expect(array.min(empty, 42u64) == 42u64);
    expect(array.max(cloned, 0u64) == 3u64);
    val words: Array[String] = ["alpha", "beta"];
    expect(array.find(words, "beta") == 1u64);
    expect(array.contains(words, "alpha"));
    expect(array.equal(words, ["alpha", "beta"]));
    expect(!array.equal(words, ["alpha", "gamma"]));
    var signed: *Array[i64] = [4i64, -3i64, 1i64];
    array.sort(signed);
    expect(signed[0] == -3i64);
    expect(array.min(signed, 0i64) == -3i64);
    expect(array.sum_i64(signed) == 2i64);
    var counter: *Counter = Counter { value = 1u64 };
    val part = array.slice(values, 1u64, 3u64) catch Array.new::[u64](0u64);
    expect(part.len == 2u64 && part[0] == 9u64 && part[1] == 9u64);
    val invalid_range = array.slice(values, 3u64, 2u64);
    match (invalid_range) { ArrayError.InvalidRange { expect(true); } else { expect(false); } }
    val combined = array.concat(values, part) catch Array.new::[u64](0u64);
    expect(combined.len == values.len + part.len && combined[4] == 9u64);
    val word_part = array.slice(words, 1u64, 2u64) catch Array.new::[String](0u64);
    expect(word_part.len == 1u64 && word_part[0] == "beta");
    val word_joined = array.concat(words, word_part) catch Array.new::[String](0u64);
    expect(word_joined.len == 3u64 && word_joined[2] == "beta");
    val objects: Array[*Counter] = [counter];
    val objects_copy = array.clone(objects);
    counter.value = 8u64;
    expect(objects_copy[0].value == 8u64);
    std.println("ok");
}
"#,
    );
}

#[test]
#[ignore = "requires NARAVM_BIN pointing to an external Naravm host"]
fn math_helpers_check_overflow_and_exponent_edges() {
    run_vl(
        r#"
use std.math.{self, MathError};
fun main() {
    expect(math.min(7u64, 3u64) == 3u64);
    expect(math.clamp(-5i64, -2i64, 2i64) == -2i64);
    expect(math.gcd_u64(0u64, 0u64) == 0u64);
    expect(math.gcd_u64(54u64, 24u64) == 6u64);
    expect((math.lcm_u64(6u64, 8u64) catch 0u64) == 24u64);
    expect((math.lcm_u64(0u64, 8u64) catch 1u64) == 0u64);
    expect((math.pow_u64(0u64, 0u64) catch 0u64) == 1u64);
    expect((math.checked_add_u64(40u64, 2u64) catch 0u64) == 42u64);
    expect((math.checked_sub_u64(42u64, 2u64) catch 0u64) == 40u64);
    expect((math.checked_div_u64(42u64, 2u64) catch 0u64) == 21u64);
    expect((math.checked_abs_i64(-42i64) catch 0i64) == 42i64);
    val add_error = math.checked_add_u64(18446744073709551615u64, 1u64);
    match (add_error) { MathError.Overflow { expect(true); } else { expect(false); } }
    val sub_error = math.checked_sub_u64(0u64, 1u64);
    match (sub_error) { MathError.Overflow { expect(true); } else { expect(false); } }
    val div_error = math.checked_div_u64(1u64, 0u64);
    match (div_error) { MathError.DivisionByZero { expect(true); } else { expect(false); } }
    val abs_error = math.checked_abs_i64(-9223372036854775807i64 - 1i64);
    match (abs_error) { MathError.Overflow { expect(true); } else { expect(false); } }
    expect((math.pow_u64(2u64, 63u64) catch 0u64) == 9223372036854775808u64);
    expect((math.pow_u64(18446744073709551615u64, 1u64) catch 0u64) == 18446744073709551615u64);
    expect((math.checked_mul_u64(18446744073709551615u64, 0u64) catch 1u64) == 0u64);
    val overflow = math.pow_u64(2u64, 64u64);
    match (overflow) { MathError.Overflow { expect(true); } else { expect(false); } }
    val lcm_overflow = math.lcm_u64(18446744073709551615u64, 2u64);
    match (lcm_overflow) { MathError.Overflow { expect(true); } else { expect(false); } }
    std.println("ok");
}
"#,
    );
}

#[test]
#[ignore = "requires NARAVM_BIN pointing to an external Naravm host"]
fn path_helpers_handle_roots_and_relative_parents() {
    run_vl(
        r#"
use std.path;
fun main() {
    expect(path.is_absolute("/a"));
    expect(!path.is_absolute("a"));
    expect(path.basename("") == "");
    expect(path.basename("/") == "/");
    expect(path.basename("///") == "/");
    expect(path.basename("a/b/") == "b");
    expect(path.dirname("") == ".");
    expect(path.dirname("a") == ".");
    expect(path.dirname("a/b") == "a");
    expect(path.dirname("a///b") == "a");
    expect(path.dirname("/file") == "/");
    expect(path.dirname("/") == "/");
    expect(path.dirname("///file") == "/");
    expect(path.dirname("/a/b/") == "/a");
    expect(path.extension("a.tar.gz") == "gz");
    expect(path.extension(".profile") == "");
    expect(path.extension("a.") == "");
    expect(path.join("a", "b") == "a/b");
    expect(path.join("a/", "b") == "a/b");
    expect(path.join("a", "/b") == "/b");
    expect(path.join("", "b") == "b");
    expect(path.join("a", "") == "a");
    std.println("ok");
}
"#,
    );
}

#[test]
#[ignore = "requires NARAVM_BIN pointing to an external Naravm host"]
fn recycled_loop_temporaries_preserve_values_over_multiple_iterations() {
    let additions = "total = total + 1u64; ".repeat(40);
    run_vl(&format!(
        "fun count(n: u64): u64 {{ var total = 0u64; var i = 0u64; while (i < n) {{ {additions} i = i + 1u64; }} return total; }} fun main() {{ expect(count(3u64) == 120u64); std.println(\"ok\"); }}"
    ));
}
