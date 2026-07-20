#!/usr/bin/env bash
# bench-sweep.sh — full evaluation sweep across interner/allocator configs.
#
# For each (interner × allocator) combination:
#   1. Build em (from portage-cli) and portage-bench criterion benches
#   2. Run em regen against the Gentoo tree (wall-clock)
#   3. Run em search for a few patterns (wall-clock)
#   4. Run criterion benches (all groups, pkgcraft included as baseline)
#
# Then run baselines once:
#   - pk repo metadata regen (pkgcraft)
#   - emerge -s / qsearch (if available on Gentoo)
#
# Finally produce an evaluation report.
#
# Usage: bench-sweep.sh [options]
#   -o, --output DIR       output directory (default: ./bench-results/<timestamp>)
#   -n, --dry-run          print commands without executing
#   --configs LIST         comma-separated configs (default: papaya-default,papaya-mimalloc,lasso-default,lasso-mimalloc,symbol-table-default,symbol-table-mimalloc)
#   --no-criterion         skip criterion benchmarks
#   --no-regen             skip regen wall-clock
#   --no-search            skip search wall-clock
#   --no-baselines         skip baseline measurements (pk, emerge, qsearch)
#   --skip-build           skip building, reuse existing binaries
#   --criterion-args ARGS  extra args to criterion (e.g. '--sample-size 20')
#   --search-patterns LIST comma-separated search patterns (default: gcc,firefox,rust)
#   --regen-jobs N         jobs for regen (default: min(nproc, 24))
#   --repo PATH            Gentoo repo path (default: ./gentoo)
#   -h, --help             show this help

set -euo pipefail

SCRIPT_DIR=$(cd "$(dirname "$(readlink -f "$0" 2>/dev/null || echo "$0")")" && pwd)
PROJ_ROOT=$(cd "$SCRIPT_DIR/.." && pwd)
CLI_ROOT=$(cd "$PROJ_ROOT/../portage-cli" 2>/dev/null && pwd || echo "")
PKGCRAFT_ROOT=$(cd "$PROJ_ROOT/../pkgcraft" 2>/dev/null && pwd || echo "")

OUTPUT=""
DRY_RUN=0
CONFIGS="papaya-default,papaya-mimalloc,lasso-default,lasso-mimalloc,symbol-table-default,symbol-table-mimalloc"
NO_CRITERION=0
NO_REGEN=0
NO_SEARCH=0
NO_BASELINES=0
SKIP_BUILD=0
CRITERION_ARGS=""
SEARCH_PATTERNS="gcc,firefox,rust"
REGEN_JOBS=""
REPO=""

usage() {
    sed -n '2,/^$/p' "$0" | sed 's/^# \{0,1\}//'
    exit 0
}

while [[ $# -gt 0 ]]; do
    case "$1" in
        -o|--output)          OUTPUT="$2"; shift 2 ;;
        -n|--dry-run)         DRY_RUN=1; shift ;;
        --configs)            CONFIGS="$2"; shift 2 ;;
        --no-criterion)       NO_CRITERION=1; shift ;;
        --no-regen)           NO_REGEN=1; shift ;;
        --no-search)          NO_SEARCH=1; shift ;;
        --no-baselines)       NO_BASELINES=1; shift ;;
        --skip-build)         SKIP_BUILD=1; shift ;;
        --criterion-args)     CRITERION_ARGS="$2"; shift 2 ;;
        --search-patterns)    SEARCH_PATTERNS="$2"; shift 2 ;;
        --regen-jobs)         REGEN_JOBS="$2"; shift 2 ;;
        --repo)               REPO="$2"; shift 2 ;;
        -h|--help)            usage ;;
        *)                    echo "unknown option: $1" >&2; exit 1 ;;
    esac
done

IFS=',' read -ra CONFIG_LIST <<< "$CONFIGS"
IFS=',' read -ra SEARCH_PATS <<< "$SEARCH_PATTERNS"

TIMESTAMP=$(date +%Y%m%d-%H%M%S)
OUTPUT="${OUTPUT:-$PROJ_ROOT/bench-results/$TIMESTAMP}"

REPO="${REPO:-$PROJ_ROOT/gentoo}"
CORES=$(nproc 2>/dev/null || sysctl -n hw.ncpu 2>/dev/null || echo 4)
REGEN_JOBS="${REGEN_JOBS:-$(awk -v c="$CORES" 'BEGIN{print (c < 24 ? c : 24)}')}"

if [[ ! -d "$REPO" ]]; then
    echo "Gentoo repo not found at $REPO" >&2
    echo "Run: git clone --depth 1 https://github.com/gentoo/gentoo.git $PROJ_ROOT/gentoo" >&2
    exit 1
fi

if [[ -z "$CLI_ROOT" || ! -f "$CLI_ROOT/Cargo.toml" ]]; then
    echo "portage-cli workspace not found at $CLI_ROOT" >&2
    echo "Run: git clone https://github.com/lu-zero/portage-cli ../portage-cli" >&2
    exit 1
fi

map_config_to_features() {
    local config="$1"
    local interner="${config%-*}"
    local alloc="${config##*-}"
    local flags=()

    case "$interner" in
        papaya)       ;;
        lasso)        flags+=("lasso") ;;
        symbol-table) flags+=("symbol-table") ;;
        *)            echo "unknown interner: $interner" >&2; return 1 ;;
    esac

    case "$alloc" in
        default) flags+=("--no-default-features") ;;
        mimalloc) ;;
        *)        echo "unknown allocator: $alloc" >&2; return 1 ;;
    esac

    if [[ ${#flags[@]} -gt 0 ]]; then
        local feature_part=""
        local no_default=""
        local has_features=0
        for f in "${flags[@]}"; do
            if [[ "$f" == "--no-default-features" ]]; then
                no_default="$f"
            else
                has_features=1
                feature_part="${feature_part:+$feature_part,}$f"
            fi
        done
        if [[ "$has_features" -eq 1 ]]; then
            echo "$no_default --features $feature_part"
        else
            echo "$no_default"
        fi
    fi
}

map_config_to_bench_features() {
    local config="$1"
    local interner="${config%-*}"
    local alloc="${config##*-}"
    local flags=()

    case "$interner" in
        papaya)       ;;
        lasso)        flags+=("lasso") ;;
        symbol-table) flags+=("symbol-table") ;;
    esac

    case "$alloc" in
        mimalloc) flags+=("mimalloc") ;;
    esac

    if [[ ${#flags[@]} -eq 0 ]]; then
        echo ""
    else
        echo "--features $(IFS=,; echo "${flags[*]}")"
    fi
}

build_em() {
    local config="$1"
    local flags
    flags=$(map_config_to_features "$config")
    local out_dir="$OUTPUT/$config"
    mkdir -p "$out_dir"

    if [[ -x "$out_dir/em" ]]; then
        echo "  em ($config): already built, skipping"
        return
    fi

    echo "  building em ($config): cargo build --release $flags"
    if [[ "$DRY_RUN" -ne 1 ]]; then
        cargo build --release $flags --manifest-path "$CLI_ROOT/Cargo.toml" 2>&1
        # CLI_ROOT/target/release/em is correct when portage-cli is its own
        # standalone workspace (the README's separate-clone setup). When
        # benchmarks/ instead lives *inside* the portage-cli monorepo as a
        # workspace member (this repo's current layout), cargo shares one
        # target/ at the enclosing workspace root instead, one level up.
        local em_bin="$CLI_ROOT/target/release/em"
        [[ -x "$em_bin" ]] || em_bin="$PROJ_ROOT/../target/release/em"
        cp "$em_bin" "$out_dir/em"
    fi
}

build_criterion() {
    local config="$1"
    local flags
    flags=$(map_config_to_bench_features "$config")

    echo "  building criterion ($config): cargo bench --no-run $flags"
    if [[ "$DRY_RUN" -ne 1 ]]; then
        cargo bench --no-run $flags --manifest-path "$PROJ_ROOT/Cargo.toml" 2>&1 | tail -1
    fi
}

time_cmd() {
    local label="$1"; shift
    local out_file="$1"; shift
    local tf
    tf=$(mktemp)

    if [[ "$(uname)" == "Darwin" ]]; then
        { /usr/bin/time -l "$@" > /dev/null; } 2>"$tf" || true
        local line real user sys rss
        line=$(grep 'real' "$tf" | head -1)
        real=$(echo "$line" | awk '{print $1}')
        user=$(echo "$line" | awk '{print $3}')
        sys=$(echo "$line" | awk '{print $5}')
        rss=$(awk '/maximum resident set size/{print $1}' "$tf")
        echo -e "$label\t$real\t$user\t$sys\t${rss:-0}" > "$out_file"
    else
        { time "$@" > /dev/null 2>&1; } 2>"$tf" || true
        local real user sys
        real=$(awk '/real/{print $2}' "$tf")
        user=$(awk '/user/{print $2}' "$tf")
        sys=$(awk '/sys/{print $2}' "$tf")
        echo -e "$label\t$real\t$user\t$sys\t0" > "$out_file"
    fi
    rm -f "$tf"
}

run_regen() {
    local config="$1"
    local em="$OUTPUT/$config/em"
    local out_dir
    out_dir=$(mktemp -d)

    echo "  regen ($config): $em --repo $REPO regen -o <tmp> -j $REGEN_JOBS"
    if [[ "$DRY_RUN" -ne 1 ]]; then
        time_cmd "$config" "$OUTPUT/$config/regen.time" \
            "$em" --repo "$REPO" regen -o "$out_dir" -j "$REGEN_JOBS"
        rm -rf "$out_dir"
    fi
}

run_search() {
    local config="$1"
    local em="$OUTPUT/$config/em"

    echo "  search ($config): patterns: ${SEARCH_PATS[*]}"
    if [[ "$DRY_RUN" -ne 1 ]]; then
        {
            echo -e "pattern\tmean_s\tmin_s"
            for pat in "${SEARCH_PATS[@]}"; do
                local total=0 min="" samples=()
                for ((i = 0; i < 3; i++)); do
                    local tf
                    tf=$(mktemp)
                    { time "$em" --repo "$REPO" search "$pat" > /dev/null 2>&1; } 2>"$tf" || true
                    local t
                    t=$(awk '/real/{
                        v=$2; sub(/m/, ":", v); sub(/s$/, "", v)
                        split(v, a, /:/)
                        print a[1]*60 + a[2]
                    }' "$tf")
                    rm -f "$tf"
                    [[ -z "$t" ]] && continue
                    samples+=("$t")
                    total=$(awk -v a="$total" -v b="$t" 'BEGIN{print a+b}')
                    if [[ -z "$min" ]] || awk -v a="$t" -v b="$min" 'BEGIN{exit (a>=b)?0:1}'; then
                        min="$t"
                    fi
                done
                local mean
                if [[ ${#samples[@]} -eq 0 ]]; then
                    mean="?"
                    min="?"
                else
                    mean=$(awk -v t="$total" -v n="${#samples[@]}" 'BEGIN{printf "%.3f", t/n}')
                fi
                echo -e "$pat\t$mean\t${min:-?}"
            done
        } > "$OUTPUT/$config/search.time"
    fi
}

run_criterion() {
    local config="$1"
    local flags
    flags=$(map_config_to_bench_features "$config")
    local out_dir="$OUTPUT/$config"
    local benches="dep_parsing realworld_dep_parsing resolve dedup"

    for bench in $benches; do
        echo "  criterion ($config): $bench"
        if [[ "$DRY_RUN" -ne 1 ]]; then
            GENTOO_REPO="$REPO" cargo bench --bench "$bench" $flags -- $CRITERION_ARGS \
                > "$out_dir/${bench}.stdout" 2> "$out_dir/${bench}.stderr" || true
            parse_criterion "$out_dir/${bench}.stdout" > "$out_dir/${bench}.parsed"
        fi
    done
}

parse_criterion() {
    local file="$1"
    if [[ ! -f "$file" || ! -s "$file" ]]; then return; fi
    awk '
    /^Gnuplot|^Executing|^Found |^Fitting|^Sampling|^Warming|^remove|^Analyzing|^Bootstrapping|^Performance|^change:/ { next }
    /^[ \t]*$/ { next }
    /^[a-zA-Z]/ && !/time:/ {
        gsub(/^[ \t]+/, "")
        if ($0 !~ /[a-zA-Z]/) { next }
        name = $0
        next
    }
    /time:/ {
        gsub(/^[ \t]+/, "")
        s = $0
        gsub(/.*time:[ \t]*\[[ \t]*/, "", s)
        split(s, parts, /[ \t\]]/)
        val = parts[1]
        unit = parts[2]
        if (val != "" && unit != "") {
            print name "\t" val "\t" unit
        }
    }
    ' "$file" 2>/dev/null || true
}

run_baselines() {
    mkdir -p "$OUTPUT/baselines"

    local pk=""
    if [[ -n "$PKGCRAFT_ROOT" && -x "$PKGCRAFT_ROOT/target/release/pk" ]]; then
        pk="$PKGCRAFT_ROOT/target/release/pk"
    elif command -v pk >/dev/null 2>&1; then
        pk=$(command -v pk)
    fi

    if [[ -n "$pk" && -x "$pk" ]]; then
        local out_dir
        out_dir=$(mktemp -d)
        echo "  baseline: pk repo metadata regen -j $REGEN_JOBS"
        if [[ "$DRY_RUN" -ne 1 ]]; then
            time_cmd "pk" "$OUTPUT/baselines/pk-regen.time" \
                "$pk" repo metadata regen -j "$REGEN_JOBS" -p "$out_dir" -n -f "$REPO"
            local pk_count
            pk_count=$(find "$out_dir" -type f 2>/dev/null | wc -l | tr -d ' ')
            echo "  pk: $pk_count files"
            rm -rf "$out_dir"
        fi
    else
        echo "  baseline: pk not found, skipping"
    fi

    if command -v emerge >/dev/null 2>&1; then
        echo "  baseline: emerge search"
        if [[ "$DRY_RUN" -ne 1 ]]; then
            {
                echo -e "pattern\tmean_s\tmin_s"
                for pat in "${SEARCH_PATS[@]}"; do
                    local total=0 min="" samples=()
                    for ((i = 0; i < 3; i++)); do
                        local tf
                        tf=$(mktemp)
                    { time emerge -s "$pat" > /dev/null 2>&1; } 2>"$tf" || true
                    local t
                    t=$(awk '/real/{print $2}' "$tf")
                        rm -f "$tf"
                        [[ -z "$t" ]] && continue
                        samples+=("$t")
                        total=$(awk -v a="$total" -v b="$t" 'BEGIN{print a+b}')
                        if [[ -z "$min" ]] || awk -v a="$t" -v b="$min" 'BEGIN{exit (a>=b)?0:1}'; then
                            min="$t"
                        fi
                    done
                    local mean
                    mean=$(awk -v t="$total" -v n="${#samples[@]}" 'BEGIN{printf "%.3f", t/n}')
                    echo -e "$pat\t$mean\t${min:-?}"
                done
            } > "$OUTPUT/baselines/emerge-search.time"
        fi
    fi
}

generate_summary() {
    local tsv="$OUTPUT/summary.tsv"
    {
        echo -e "type\tconfig\tbench_group\tbench_name\tmetric\tvalue\tunit"

        for config in "${CONFIG_LIST[@]}"; do
            local dir="$OUTPUT/$config"

            for bench in dep_parsing realworld_dep_parsing resolve dedup; do
                if [[ -f "$dir/${bench}.parsed" ]]; then
                    while IFS=$'\t' read -r name median unit; do
                        echo -e "criterion\t$config\t$bench\t$name\tmedian\t$median\t$unit"
                    done < "$dir/${bench}.parsed"
                fi
            done

            if [[ -f "$dir/regen.time" ]]; then
                IFS=$'\t' read -r _ real user sys rss < "$dir/regen.time"
                real="${real%s}"; user="${user%s}"; sys="${sys%s}"
                real="${real#0m}"; user="${user#0m}"; sys="${sys#0m}"
                echo -e "regen\t$config\tregen\tregen\treal\t${real}\ts"
                echo -e "regen\t$config\tregen\tregen\tuser\t${user}\ts"
                echo -e "regen\t$config\tregen\tregen\tsys\t${sys}\ts"
                echo -e "regen\t$config\tregen\tregen\trss\t${rss}\tkB"
            fi

            if [[ -f "$dir/search.time" ]]; then
                tail -n +2 "$dir/search.time" | while IFS=$'\t' read -r pat mean min; do
                    echo -e "search\t$config\tsearch\t$pat\tmean\t${mean}\ts"
                    echo -e "search\t$config\tsearch\t$pat\tmin\t${min}\ts"
                done
            fi
        done

        if [[ -f "$OUTPUT/baselines/pk-regen.time" ]]; then
            IFS=$'\t' read -r _ real user sys rss < "$OUTPUT/baselines/pk-regen.time"
            real="${real%s}"; user="${user%s}"; sys="${sys%s}"
            real="${real#0m}"; user="${user#0m}"; sys="${sys#0m}"
            echo -e "regen\tpk\tregen\tregen\treal\t${real}\ts"
            echo -e "regen\tpk\tregen\tregen\tuser\t${user}\ts"
            echo -e "regen\tpk\tregen\tregen\tsys\t${sys}\ts"
            echo -e "regen\tpk\tregen\tregen\trss\t${rss}\tkB"
        fi

        if [[ -f "$OUTPUT/baselines/emerge-search.time" ]]; then
            tail -n +2 "$OUTPUT/baselines/emerge-search.time" | while IFS=$'\t' read -r pat mean min; do
                echo -e "search\temerge\tsearch\t$pat\tmean\t${mean}\ts"
                echo -e "search\temerge\tsearch\t$pat\tmin\t${min}\ts"
            done
        fi
    } > "$tsv"
}

generate_report() {
    local report="$OUTPUT/report.md"
    "$SCRIPT_DIR/bench-eval.sh" "$OUTPUT/summary.tsv" > "$report" 2>/dev/null || {
        echo "WARNING: bench-eval.sh failed, skipping report generation" >&2
        return
    }
    echo "report: $report"
}

echo "=== portage-bench sweep ==="
echo "output:   $OUTPUT"
echo "repo:     $REPO"
echo "configs:  ${CONFIG_LIST[*]}"
echo "cores:    $CORES  (regen jobs: $REGEN_JOBS)"
echo "em:       $CLI_ROOT"
echo ""

mkdir -p "$OUTPUT"

{
    echo "timestamp=$TIMESTAMP"
    echo "uname=$(uname -a)"
    echo "rustc=$(rustc --version 2>/dev/null || echo unknown)"
    echo "cpu=$(sysctl -n machdep.cpu.brand_string 2>/dev/null || lscpu 2>/dev/null | grep 'Model name' | head -1 | cut -d: -f2 | xargs || echo unknown)"
    echo "cores=$CORES"
    echo "regen_jobs=$REGEN_JOBS"
    echo "configs=${CONFIG_LIST[*]}"
    echo "repo=$REPO"
} > "$OUTPUT/meta.env"

if [[ "$SKIP_BUILD" -ne 1 ]]; then
    for config in "${CONFIG_LIST[@]}"; do
        mkdir -p "$OUTPUT/$config"
        echo ""
        echo "--- build: $config ---"
        build_em "$config"
        if [[ "$NO_CRITERION" -ne 1 ]]; then
            build_criterion "$config"
        fi
    done
else
    echo ""
    echo "--- build: skipped (--skip-build) ---"
    for config in "${CONFIG_LIST[@]}"; do
        mkdir -p "$OUTPUT/$config"
    done
fi

if [[ "$NO_BASELINES" -ne 1 ]]; then
    echo ""
    echo "--- baselines ---"
    run_baselines
fi

for config in "${CONFIG_LIST[@]}"; do
    echo ""
    echo "--- run: $config ---"

    if [[ "$NO_REGEN" -ne 1 ]]; then
        run_regen "$config"
    fi

    if [[ "$NO_SEARCH" -ne 1 ]]; then
        run_search "$config"
    fi

    if [[ "$NO_CRITERION" -ne 1 ]]; then
        run_criterion "$config"
    fi
done

echo ""
echo "--- summary ---"
generate_summary
echo "summary: $OUTPUT/summary.tsv"

if [[ "$DRY_RUN" -ne 1 ]]; then
    generate_report
fi

echo ""
echo "=== done ==="
