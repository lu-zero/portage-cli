//! Integration tests for bash constructs commonly used by Gentoo eclasses.
//!
//! These tests verify that brush-core handles the specific bash patterns
//! found in Gentoo's eclass ecosystem.  Failures here indicate brush-core
//! limitations that cause metadata extraction mismatches in `regen_cache`.

use portage_repo::{EbuildShell, Repository};
use tempfile::TempDir;

/// Create a minimal repository and shell for testing.
async fn test_shell() -> (TempDir, EbuildShell) {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();

    // Minimal repo structure
    std::fs::create_dir_all(root.join("metadata")).unwrap();
    std::fs::write(root.join("metadata/layout.conf"), "masters =\n").unwrap();
    std::fs::create_dir_all(root.join("profiles")).unwrap();
    std::fs::write(root.join("profiles/repo_name"), "test-repo\n").unwrap();

    let repo = Repository::builder().in_memory_cache().open(root).unwrap();
    let shell = repo.shell().await.unwrap();
    (tmp, shell)
}

/// Source a bash script string in the shell and return the value of __OUT.
async fn eval_var(shell: &mut EbuildShell, script: &str) -> String {
    let tmp = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(tmp.path(), script).unwrap();
    let _ = shell.source_make_defaults(tmp.path()).await;
    shell.get_var("__OUT").unwrap_or_default()
}

// ─── 1. Default assignment via no-op command ────────────────────────
//
// Pattern:  : "${VAR:=default}"
// Used by:  acct-group.eclass, acct-user.eclass, mate-desktop.org.eclass,
//           gstreamer-meson.eclass, out-of-source.eclass
// Impact:   ~9,716 "missing DESCRIPTION" errors

#[tokio::test]
async fn noop_default_assignment_unset() {
    let (_tmp, mut shell) = test_shell().await;
    let got = eval_var(
        &mut shell,
        r#"
        unset MYVAR
        : "${MYVAR:=hello world}"
        __OUT="${MYVAR}"
        "#,
    )
    .await;
    assert_eq!(
        got, "hello world",
        ": \"${{VAR:=value}}\" should set unset var"
    );
}

#[tokio::test]
async fn noop_default_assignment_empty() {
    let (_tmp, mut shell) = test_shell().await;
    let got = eval_var(
        &mut shell,
        r#"
        MYVAR=""
        : "${MYVAR:=fallback}"
        __OUT="${MYVAR}"
        "#,
    )
    .await;
    assert_eq!(
        got, "fallback",
        ": \"${{VAR:=value}}\" should set empty var"
    );
}

#[tokio::test]
async fn noop_default_assignment_already_set() {
    let (_tmp, mut shell) = test_shell().await;
    let got = eval_var(
        &mut shell,
        r#"
        MYVAR="existing"
        : "${MYVAR:=fallback}"
        __OUT="${MYVAR}"
        "#,
    )
    .await;
    assert_eq!(
        got, "existing",
        ": \"${{VAR:=value}}\" should not overwrite existing var"
    );
}

/// Nested double quotes with expansion inside ${:=}.
/// `: "${VAR:="text ${EXPANSION}"}"` — previously broke the parser,
/// now fixed in the winnow parser.
#[tokio::test]
async fn noop_default_assignment_with_expansion() {
    let (_tmp, mut shell) = test_shell().await;
    let got = eval_var(
        &mut shell,
        r#"
        PN="testpkg"
        : "${DESCRIPTION:="System group: ${PN}"}"
        __OUT="${DESCRIPTION}"
        "#,
    )
    .await;
    assert_eq!(
        got, "System group: testpkg",
        ": \"${{VAR:=value}}\" should expand nested variables"
    );
}

// Narrowing down: does the inner expansion work without nested quotes?
#[tokio::test]
async fn noop_default_assignment_expansion_no_inner_quotes() {
    let (_tmp, mut shell) = test_shell().await;
    let got = eval_var(
        &mut shell,
        r#"
        PN="testpkg"
        : "${DESCRIPTION:=System group: ${PN}}"
        __OUT="${DESCRIPTION}"
        "#,
    )
    .await;
    assert_eq!(
        got, "System group: testpkg",
        ": \"${{VAR:=text ${{PN}}}}\" without inner quotes should work"
    );
}

// Does it fail only with nested double quotes inside :=?
#[tokio::test]
async fn noop_default_assignment_nested_double_quotes() {
    let (_tmp, mut shell) = test_shell().await;
    let got = eval_var(
        &mut shell,
        r#"
        : "${MYVAR:="hello"}"
        __OUT="${MYVAR}"
        "#,
    )
    .await;
    assert_eq!(
        got, "hello",
        ": \"${{VAR:=\"literal\"}}\" nested double quotes should work"
    );
}

/// Nested double quotes with space inside ${:=}.
/// `: "${VAR:="hello world"}"` — previously broke the parser,
/// now fixed in the winnow parser.
#[tokio::test]
async fn noop_default_assignment_nested_quotes_concat() {
    let (_tmp, mut shell) = test_shell().await;
    let got = eval_var(
        &mut shell,
        r#"
        : "${MYVAR:="hello world"}"
        __OUT="${MYVAR}"
        "#,
    )
    .await;
    assert_eq!(
        got, "hello world",
        ": \"${{VAR:=\"hello world\"}}\" nested double quotes with space should work"
    );
}

// ─── 2. Array parameter expansion with suffix removal ───────────────
//
// Pattern:  "${array[@]%:*}"
// Used by:  multilib-build.eclass, app-alternatives.eclass, sec-keys.eclass
// Impact:   ~634 IUSE mismatches (multilib abi_* flags)

#[tokio::test]
async fn array_suffix_removal() {
    let (_tmp, mut shell) = test_shell().await;
    let got = eval_var(
        &mut shell,
        r#"
        ITEMS=("abi_x86_32:x86" "abi_x86_64:amd64" "abi_mips_n32:mips")
        RESULT=( "${ITEMS[@]%:*}" )
        __OUT="${RESULT[*]}"
        "#,
    )
    .await;
    assert_eq!(
        got, "abi_x86_32 abi_x86_64 abi_mips_n32",
        "\"${{array[@]%:*}}\" should remove suffix from each element"
    );
}

#[tokio::test]
async fn array_greedy_suffix_removal() {
    let (_tmp, mut shell) = test_shell().await;
    let got = eval_var(
        &mut shell,
        r#"
        ITEMS=("a:b:c" "x:y:z")
        RESULT=( "${ITEMS[@]%%:*}" )
        __OUT="${RESULT[*]}"
        "#,
    )
    .await;
    assert_eq!(
        got, "a x",
        "\"${{array[@]%%:*}}\" should greedily remove suffix from each element"
    );
}

// ─── 3. Array parameter expansion with suffix append ────────────────
//
// Pattern:  "${array[@]/%/suffix}"
// Used by:  multilib-build.eclass, python-r1.eclass, lua.eclass, ada.eclass
// Impact:   IUSE, REQUIRED_USE computation for many eclasses

#[tokio::test]
async fn array_suffix_append() {
    let (_tmp, mut shell) = test_shell().await;
    let got = eval_var(
        &mut shell,
        r#"
        FLAGS=("flag1" "flag2" "flag3")
        RESULT=( "${FLAGS[@]/%/(-)?}" )
        __OUT="${RESULT[*]}"
        "#,
    )
    .await;
    assert_eq!(
        got, "flag1(-)? flag2(-)? flag3(-)?",
        "\"${{array[@]/%/suffix}}\" should append to each element"
    );
}

// ─── 4. Array parameter expansion with prefix prepend ───────────────
//
// Pattern:  "${array[@]/#/prefix}"
// Used by:  lua.eclass, llvm-r2.eclass, lua-single.eclass, wine.eclass
// Impact:   USE flag target computation

#[tokio::test]
async fn array_prefix_prepend() {
    let (_tmp, mut shell) = test_shell().await;
    let got = eval_var(
        &mut shell,
        r#"
        IMPLS=("lua51" "lua52" "lua53")
        RESULT=( "${IMPLS[@]/#/lua_targets_}" )
        __OUT="${RESULT[*]}"
        "#,
    )
    .await;
    assert_eq!(
        got, "lua_targets_lua51 lua_targets_lua52 lua_targets_lua53",
        "\"${{array[@]/#/prefix}}\" should prepend to each element"
    );
}

// ─── 5. Array in for loop with parameter expansion ──────────────────
//
// Pattern:  for x in "${array[@]%:*}"; do ...; done
// Used by:  app-alternatives.eclass, sec-keys.eclass

#[tokio::test]
async fn for_loop_array_suffix_removal() {
    let (_tmp, mut shell) = test_shell().await;
    let got = eval_var(
        &mut shell,
        r#"
        ITEMS=("one:1" "two:2" "three:3")
        __OUT=""
        for item in "${ITEMS[@]%:*}"; do
            __OUT="${__OUT:+${__OUT} }${item}"
        done
        "#,
    )
    .await;
    assert_eq!(
        got, "one two three",
        "for loop with \"${{array[@]%:*}}\" should iterate stripped elements"
    );
}

// ─── 6. readonly arrays ─────────────────────────────────────────────
//
// Pattern:  readonly ARRAY
// Used by:  multilib-build.eclass (_MULTILIB_FLAGS)
// Impact:   needed for multilib-build to function correctly

#[tokio::test]
async fn readonly_array() {
    let (_tmp, mut shell) = test_shell().await;
    let got = eval_var(
        &mut shell,
        r#"
        FLAGS=("a" "b" "c")
        readonly FLAGS
        __OUT="${FLAGS[*]}"
        "#,
    )
    .await;
    assert_eq!(got, "a b c", "readonly array should preserve its values");
}

// ─── 6b. Multiline array with comments (real _MULTILIB_FLAGS pattern) ─
//
// Pattern:  TAB-indented entries, with #-commented lines inside
// Used by:  multilib-build.eclass (_MULTILIB_FLAGS)
// Impact:   empty _MULTILIB_FLAGS → empty IUSE for all multilib packages

#[tokio::test]
async fn multiline_array_with_comments() {
    let (_tmp, mut shell) = test_shell().await;
    // Use tabs like the real eclass does
    let got = eval_var(
        &mut shell,
        "_MULTILIB_FLAGS=(\n\tabi_x86_32:x86\n\tabi_x86_64:amd64\n#\tabi_ppc_32:ppc\n#\tabi_ppc_64:ppc64\n\tabi_s390_32:s390\n)\n__OUT=\"${#_MULTILIB_FLAGS[@]} ${_MULTILIB_FLAGS[0]} ${_MULTILIB_FLAGS[2]}\"\n",
    )
    .await;
    assert_eq!(
        got, "3 abi_x86_32:x86 abi_s390_32:s390",
        "multiline array with comments should have 3 elements"
    );
}

#[tokio::test]
async fn multiline_array_8_entries() {
    let (_tmp, mut shell) = test_shell().await;
    let got = eval_var(
        &mut shell,
        "_MULTILIB_FLAGS=(\n\tabi_x86_32:x86,x86_fbsd\n\tabi_x86_64:amd64,amd64_fbsd\n\tabi_x86_x32:x32\n\tabi_mips_n32:n32\n\tabi_mips_n64:n64\n\tabi_mips_o32:o32\n\tabi_s390_32:s390\n\tabi_s390_64:s390x\n)\nreadonly _MULTILIB_FLAGS\n__OUT=\"${#_MULTILIB_FLAGS[@]}\"\n",
    )
    .await;
    assert_eq!(
        got, "8",
        "multiline array with 8 entries should have count=8"
    );
}

// ─── 6c. Array defined in sourced file via inherit mechanism ─────────
//
// Tests whether arrays assigned inside a `source`'d file (as happens
// with eclass sourcing via `inherit`) retain their values after
// a nested `source` call.

#[tokio::test]
async fn array_survives_nested_source() {
    let (_tmp, mut shell) = test_shell().await;

    // Create two files: inner.sh defines an array, then sources nested.sh,
    // then a function reads the array.
    let dir = _tmp.path();
    let inner = dir.join("inner.sh");
    let nested = dir.join("nested.sh");

    // nested.sh does nothing interesting
    std::fs::write(&nested, "NESTED_VAR=1\n").unwrap();

    // inner.sh defines an array, sources nested.sh, then reads the array
    std::fs::write(
        &inner,
        format!(
            r#"
MY_ARRAY=(
	item1:val1
	item2:val2
	item3:val3
)
readonly MY_ARRAY
source "{nested}"
_set_result() {{
    local flags=( "${{MY_ARRAY[@]%:*}}" )
    __OUT="${{#MY_ARRAY[@]}} ${{flags[*]}}"
}}
_set_result
unset -f _set_result
"#,
            nested = nested.display()
        ),
    )
    .unwrap();

    let _ = shell.source_make_defaults(&inner).await;
    let got = shell.get_var("__OUT").unwrap_or_default();
    assert_eq!(
        got, "3 item1 item2 item3",
        "array should survive nested source and be readable in function"
    );
}

// ─── 7. local array with function-scope parameter expansion ─────────
//
// Pattern:  local flags=( "${ARRAY[@]%:*}" )
// Used by:  multilib-build.eclass (_multilib_build_set_globals)
// Impact:   direct cause of missing abi_* IUSE flags

#[tokio::test]
async fn local_array_from_expansion() {
    let (_tmp, mut shell) = test_shell().await;
    let got = eval_var(
        &mut shell,
        r#"
        _ITEMS=("abi_x86_32:x86" "abi_x86_64:amd64")
        readonly _ITEMS

        test_func() {
            local flags=( "${_ITEMS[@]%:*}" )
            IUSE=${flags[*]}
        }
        test_func
        __OUT="${IUSE}"
        "#,
    )
    .await;
    assert_eq!(
        got, "abi_x86_32 abi_x86_64",
        "local array from parameter expansion in function should work"
    );
}

// ─── 8. Conditional default assignment ──────────────────────────────
//
// Pattern:  : "${VAR:=yes}"  then  [[ ${VAR} != "no" ]] && BDEPEND=...
// Used by:  gnuconfig.eclass, autotools.eclass
// Impact:   BDEPEND accumulation

#[tokio::test]
async fn conditional_default_and_test() {
    let (_tmp, mut shell) = test_shell().await;
    let got = eval_var(
        &mut shell,
        r#"
        : "${AUTO_DEPEND:=yes}"
        [[ ${AUTO_DEPEND} != "no" ]] && RESULT="deps-here"
        __OUT="${RESULT}"
        "#,
    )
    .await;
    assert_eq!(
        got, "deps-here",
        "default assignment + conditional should work together"
    );
}

// ─── 9. unset -f (function removal) ────────────────────────────────
//
// Pattern:  func() { ...; }; func; unset -f func
// Used by:  multilib-build.eclass, python-utils-r1.eclass
// Impact:   cleanup pattern used by many eclasses

#[tokio::test]
async fn unset_function() {
    let (_tmp, mut shell) = test_shell().await;
    let got = eval_var(
        &mut shell,
        r#"
        _setup() { MYVAR="from_func"; }
        _setup
        unset -f _setup
        __OUT="${MYVAR}"
        "#,
    )
    .await;
    assert_eq!(
        got, "from_func",
        "function should run before unset -f, var should persist"
    );
}

// ─── 10. Complex multilib-build pattern (end-to-end) ────────────────
//
// This is the minimal reproduction of multilib-build.eclass's
// _multilib_build_set_globals function.

#[tokio::test]
async fn multilib_build_set_globals_pattern() {
    let (_tmp, mut shell) = test_shell().await;
    let got = eval_var(
        &mut shell,
        r#"
        _MULTILIB_FLAGS=(
            abi_x86_32:x86
            abi_x86_64:amd64
        )
        readonly _MULTILIB_FLAGS

        _multilib_build_set_globals() {
            local flags=( "${_MULTILIB_FLAGS[@]%:*}" )
            local usedeps=${flags[@]/%/(-)?}
            IUSE=${flags[*]}
            MULTILIB_USEDEP=${usedeps// /,}
        }
        _multilib_build_set_globals
        unset -f _multilib_build_set_globals
        __OUT="${IUSE}|${MULTILIB_USEDEP}"
        "#,
    )
    .await;
    assert_eq!(
        got, "abi_x86_32 abi_x86_64|abi_x86_32(-)?,abi_x86_64(-)?",
        "multilib-build _set_globals pattern should produce correct IUSE and USEDEP"
    );
}

// ─── llvm-r1.eclass: _llvm_set_globals pattern ─────────────────────────────
//
// Two distinct brush behaviours are exercised:
//   1. Brace expansion with two-digit bounds: ( {16..21} )
//   2. Array element deletion via negative index: unset 'arr[-1]'
//
// Impact: wrong IUSE for every package using llvm-r1.eclass (e.g. postgresql)

#[tokio::test]
async fn brace_expansion_two_digit() {
    let (_tmp, mut shell) = test_shell().await;
    let got = eval_var(
        &mut shell,
        r#"
        arr=( {16..21} )
        __OUT="${arr[@]}"
        "#,
    )
    .await;
    assert_eq!(
        got, "16 17 18 19 20 21",
        "brace expansion {{16..21}} must include 16"
    );
}

#[tokio::test]
async fn array_unset_negative_index() {
    let (_tmp, mut shell) = test_shell().await;
    let got = eval_var(
        &mut shell,
        r#"
        arr=(a b c d)
        unset 'arr[-1]'
        __OUT="${arr[@]}"
        "#,
    )
    .await;
    assert_eq!(got, "a b c", "unset 'arr[-1]' must remove the last element");
}

#[tokio::test]
async fn llvm_set_globals_pattern() {
    let (_tmp, mut shell) = test_shell().await;
    let got = eval_var(
        &mut shell,
        r#"
        LLVM_COMPAT=( {16..21} )
        _LLVM_NEWEST_STABLE=21
        _LLVM_OLDEST_SLOT=15

        stable=() unstable=()
        for x in "${LLVM_COMPAT[@]}"; do
            if [[ ${x} -gt ${_LLVM_NEWEST_STABLE} ]]; then
                unstable+=( "${x}" )
            elif [[ ${x} -ge ${_LLVM_OLDEST_SLOT} ]]; then
                stable+=( "${x}" )
            fi
        done

        IUSE="+llvm_slot_${stable[-1]}"
        unset 'stable[-1]'
        nondefault=( "${stable[@]}" "${unstable[@]}" )
        IUSE+=" ${nondefault[*]/#/llvm_slot_}"
        __OUT="${IUSE}"
        "#,
    )
    .await;
    assert_eq!(
        got, "+llvm_slot_21 llvm_slot_16 llvm_slot_17 llvm_slot_18 llvm_slot_19 llvm_slot_20",
        "llvm_set_globals pattern must produce correct IUSE with all slots"
    );
}

// ─── 11. declare -n namerefs ────────────────────────────────────────
//
// Pattern:  declare -n ref=target; ... $ref ...; ref="value"
// Used by:  nginx.eclass (_ngx_set_mod_depend)
// Impact:   BDEPEND/DEPEND/RDEPEND for www-servers/nginx packages

/// Reading through a nameref must expand to the target variable's value.
#[tokio::test]
async fn nameref_scalar_read() {
    let (_tmp, mut shell) = test_shell().await;
    let got = eval_var(
        &mut shell,
        r#"
        TARGET="hello_world"
        declare -n REF=TARGET
        __OUT="${REF}"
        "#,
    )
    .await;
    assert_eq!(
        got, "hello_world",
        "reading a nameref should return the target's value"
    );
}

/// Writing to a nameref must update the target variable, not the nameref itself.
#[tokio::test]
async fn nameref_scalar_write() {
    let (_tmp, mut shell) = test_shell().await;
    let got = eval_var(
        &mut shell,
        r#"
        TARGET="original"
        declare -n REF=TARGET
        REF="modified"
        __OUT="${TARGET}"
        "#,
    )
    .await;
    assert_eq!(
        got, "modified",
        "writing to a nameref must update the target variable"
    );
}

/// `declare -n var` with no `=` makes var a nameref pointing to its current value
/// (the name it currently holds becomes the target name).
/// Used by nginx.eclass: `for dep_type in DEPEND RDEPEND; do declare -n dep_type; done`
#[tokio::test]
async fn nameref_no_equals_self_redirect() {
    let (_tmp, mut shell) = test_shell().await;
    let got = eval_var(
        &mut shell,
        r#"
        ACTUAL="the_value"
        dep_type="ACTUAL"
        declare -n dep_type
        __OUT="${dep_type}"
        "#,
    )
    .await;
    assert_eq!(
        got, "the_value",
        "declare -n var (no =) should make var a nameref to its current value"
    );
}

/// Appending via `+=` through a nameref must append to the target variable.
#[tokio::test]
async fn nameref_append_write() {
    let (_tmp, mut shell) = test_shell().await;
    let got = eval_var(
        &mut shell,
        r#"
        TARGET="hello"
        declare -n REF=TARGET
        REF+=" world"
        __OUT="${TARGET}"
        "#,
    )
    .await;
    assert_eq!(
        got, "hello world",
        "appending via nameref must update the target"
    );
}

/// Reading an element of an associative array through a nameref.
#[tokio::test]
async fn nameref_assoc_array_element_read() {
    let (_tmp, mut shell) = test_shell().await;
    let got = eval_var(
        &mut shell,
        r#"
        declare -A TABLE=([key1]="val1" [key2]="val2")
        declare -n REF=TABLE
        __OUT="${REF[key1]}"
        "#,
    )
    .await;
    assert_eq!(
        got, "val1",
        "nameref to assoc array must allow element reads via [key]"
    );
}

/// Iterating keys of an associative array through a nameref with `${!ref[@]}`.
#[tokio::test]
async fn nameref_assoc_array_key_iteration() {
    let (_tmp, mut shell) = test_shell().await;
    let got = eval_var(
        &mut shell,
        r#"
        declare -A TABLE=([only_key]="only_val")
        declare -n REF=TABLE
        keys=( "${!REF[@]}" )
        __OUT="${#keys[@]}:${keys[0]}"
        "#,
    )
    .await;
    assert_eq!(
        got, "1:only_key",
        "nameref key iteration via nameref must return the target assoc array's keys"
    );
}

/// `declare +n` must remove the nameref attribute so subsequent writes go to
/// the nameref variable itself rather than its former target.
#[tokio::test]
async fn nameref_removed_by_declare_plus_n() {
    let (_tmp, mut shell) = test_shell().await;
    let got = eval_var(
        &mut shell,
        r#"
        TARGET="target_value"
        declare -n REF=TARGET
        declare +n REF
        REF="direct_value"
        __OUT="${TARGET}|${REF}"
        "#,
    )
    .await;
    assert_eq!(
        got, "target_value|direct_value",
        "declare +n must remove nameref: subsequent write goes to REF, not TARGET"
    );
}

/// Minimal reproduction of the nginx.eclass nameref accumulation pattern for BDEPEND:
/// nameref to assoc array + nameref-to-self (via `declare -n var`) in a for loop
/// at global scope.  This covers the case where brush fixed the BDEPEND mismatches.
///
/// Note: the function-scope variant has a bash 5.3 regression (`dep_type+=` on
/// a function-local nameref emits "not a valid identifier") that affects bash itself
/// on macOS; only the global-scope pattern is tested here.
#[tokio::test]
async fn nameref_nginx_dep_accumulation_global() {
    let (_tmp, mut shell) = test_shell().await;
    let got = eval_var(
        &mut shell,
        r#"
        declare -A _NGX_MOD_BDEPEND=([http_perl]="dev-lang/perl")
        BDEPEND=""

        for dep_type in BDEPEND; do
            declare -n dep_table="_NGX_MOD_${dep_type}"
            declare -n dep_type
            for mod in "${!dep_table[@]}"; do
                dep_type+=" nginx_modules_${mod}? ( ${dep_table[${mod}]} )"
            done
            declare +n dep_table dep_type
        done

        __OUT="${BDEPEND}"
        "#,
    )
    .await;
    assert_eq!(
        got.trim(),
        "nginx_modules_http_perl? ( dev-lang/perl )",
        "nginx-style global nameref dep accumulation must write through nameref to BDEPEND"
    );
}

// ─── Inherit accumulation (B_*/E_* pattern, PMS 10.2) ──────────────────
//
// Tests for the metadata variable accumulation logic in the inherit builtin.
// Each eclass must see empty accumulating vars; its contributions are captured
// into E_* variables and the prior B_* values are restored.

/// Create a minimal repo with an eclass directory for inherit tests.
async fn inherit_shell() -> (TempDir, EbuildShell) {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();

    std::fs::create_dir_all(root.join("metadata")).unwrap();
    std::fs::write(root.join("metadata/layout.conf"), "masters =\n").unwrap();
    std::fs::create_dir_all(root.join("profiles")).unwrap();
    std::fs::write(root.join("profiles/repo_name"), "test-repo\n").unwrap();
    std::fs::create_dir_all(root.join("eclass")).unwrap();

    let repo = Repository::builder().in_memory_cache().open(root).unwrap();
    let shell = repo.shell().await.unwrap();
    (tmp, shell)
}

/// Source a script string via `source_make_defaults` and return `__OUT`.
async fn inherit_eval(shell: &mut EbuildShell, script: &str) -> String {
    let tmp = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(tmp.path(), script).unwrap();
    let _ = shell.source_make_defaults(tmp.path()).await;
    shell.get_var("__OUT").unwrap_or_default()
}

#[tokio::test]
async fn inherit_single_eclass_captures_iuse() {
    let (tmp, mut shell) = inherit_shell().await;

    // eclass that sets IUSE
    std::fs::write(
        tmp.path().join("eclass/foo.eclass"),
        "IUSE=\"ssl threads\"\n",
    )
    .unwrap();

    let got = inherit_eval(
        &mut shell,
        r#"
        EAPI=8
        inherit foo
        __OUT="E_IUSE=${E_IUSE} IUSE=${IUSE}"
        "#,
    )
    .await;
    assert_eq!(
        got, "E_IUSE=ssl threads IUSE=",
        "single eclass: E_IUSE captures contribution, IUSE restored to empty"
    );
}

#[tokio::test]
async fn inherit_two_eclasses_accumulate() {
    let (tmp, mut shell) = inherit_shell().await;

    std::fs::write(tmp.path().join("eclass/foo.eclass"), "IUSE=\"ssl\"\n").unwrap();
    std::fs::write(tmp.path().join("eclass/bar.eclass"), "IUSE=\"threads\"\n").unwrap();

    let got = inherit_eval(
        &mut shell,
        r#"
        EAPI=8
        inherit foo bar
        __OUT="E_IUSE=${E_IUSE}"
        "#,
    )
    .await;
    assert_eq!(
        got, "E_IUSE=ssl threads",
        "two eclasses: E_IUSE accumulates both contributions"
    );
}

#[tokio::test]
async fn inherit_accumulates_depend_and_bdepend() {
    let (tmp, mut shell) = inherit_shell().await;

    std::fs::write(
        tmp.path().join("eclass/foo.eclass"),
        "DEPEND=\"sys-libs/zlib\"\nBDEPEND=\"dev-util/cmake\"\n",
    )
    .unwrap();
    std::fs::write(
        tmp.path().join("eclass/bar.eclass"),
        "DEPEND=\"dev-libs/openssl\"\n",
    )
    .unwrap();

    let got = inherit_eval(
        &mut shell,
        r#"
        EAPI=7
        inherit foo bar
        __OUT="E_DEPEND=${E_DEPEND} E_BDEPEND=${E_BDEPEND}"
        "#,
    )
    .await;
    assert_eq!(
        got, "E_DEPEND=sys-libs/zlib dev-libs/openssl E_BDEPEND=dev-util/cmake",
        "DEPEND and BDEPEND accumulate independently"
    );
}

#[tokio::test]
async fn inherit_eapi7_excludes_properties_restrict() {
    let (tmp, mut shell) = inherit_shell().await;

    std::fs::write(
        tmp.path().join("eclass/foo.eclass"),
        "PROPERTIES=\"live\"\nRESTRICT=\"test\"\nIUSE=\"ssl\"\n",
    )
    .unwrap();

    let got = inherit_eval(
        &mut shell,
        r#"
        EAPI=7
        inherit foo
        __OUT="E_PROPERTIES=${E_PROPERTIES:-unset} E_RESTRICT=${E_RESTRICT:-unset} E_IUSE=${E_IUSE}"
        "#,
    )
    .await;
    assert_eq!(
        got, "E_PROPERTIES=unset E_RESTRICT=unset E_IUSE=ssl",
        "EAPI 7: PROPERTIES/RESTRICT are not accumulated (no E_PROPERTIES/E_RESTRICT vars)"
    );
}

#[tokio::test]
async fn inherit_eapi8_includes_properties_restrict() {
    let (tmp, mut shell) = inherit_shell().await;

    std::fs::write(
        tmp.path().join("eclass/foo.eclass"),
        "PROPERTIES=\"live\"\nRESTRICT=\"test\"\nIUSE=\"ssl\"\n",
    )
    .unwrap();

    let got = inherit_eval(
        &mut shell,
        r#"
        EAPI=8
        inherit foo
        __OUT="E_PROPERTIES=${E_PROPERTIES} E_RESTRICT=${E_RESTRICT} E_IUSE=${E_IUSE}"
        "#,
    )
    .await;
    assert_eq!(
        got, "E_PROPERTIES=live E_RESTRICT=test E_IUSE=ssl",
        "EAPI 8: PROPERTIES/RESTRICT are accumulated into E_PROPERTIES/E_RESTRICT"
    );
}

#[tokio::test]
async fn inherit_nested_eclass() {
    let (tmp, mut shell) = inherit_shell().await;

    // bar.eclass inherits baz internally
    std::fs::write(tmp.path().join("eclass/baz.eclass"), "IUSE=\"debug\"\n").unwrap();
    std::fs::write(
        tmp.path().join("eclass/bar.eclass"),
        "inherit baz\nIUSE=\"threads\"\n",
    )
    .unwrap();

    let got = inherit_eval(
        &mut shell,
        r#"
        EAPI=8
        inherit bar
        __OUT="E_IUSE=${E_IUSE} INHERITED=${INHERITED}"
        "#,
    )
    .await;
    assert_eq!(
        got, "E_IUSE=debug threads INHERITED=baz bar",
        "nested inherit: baz's IUSE captured first, then bar's, INHERITED records order"
    );
}

#[tokio::test]
async fn inherit_skips_already_inherited() {
    let (tmp, mut shell) = inherit_shell().await;

    std::fs::write(tmp.path().join("eclass/foo.eclass"), "IUSE=\"ssl\"\n").unwrap();

    let got = inherit_eval(
        &mut shell,
        r#"
        EAPI=8
        inherit foo
        IUSE="${IUSE} extra"
        inherit foo
        __OUT="E_IUSE=${E_IUSE} IUSE=${IUSE}"
        "#,
    )
    .await;
    assert_eq!(
        got, "E_IUSE=ssl IUSE= extra",
        "second inherit of same eclass is a no-op for sourcing but INHERIT still records it"
    );
}

#[tokio::test]
async fn inherit_inherit_var_records_direct_only() {
    let (tmp, mut shell) = inherit_shell().await;

    std::fs::write(tmp.path().join("eclass/baz.eclass"), "IUSE=\"debug\"\n").unwrap();
    std::fs::write(
        tmp.path().join("eclass/bar.eclass"),
        "inherit baz\nIUSE=\"threads\"\n",
    )
    .unwrap();
    std::fs::write(tmp.path().join("eclass/foo.eclass"), "IUSE=\"ssl\"\n").unwrap();

    let got = inherit_eval(
        &mut shell,
        r#"
        EAPI=8
        inherit foo bar
        __OUT="INHERIT=${INHERIT} INHERITED=${INHERITED}"
        "#,
    )
    .await;
    assert_eq!(
        got, "INHERIT=foo bar INHERITED=foo baz bar",
        "INHERIT has direct eclasses only, INHERITED has all transitively"
    );
}
