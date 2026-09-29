# rucc-postgres

A harness that builds a pinned, unmodified PostgreSQL release with [rucc](https://github.com/tamnd/rucc) and with a reference compiler, runs Postgres's own test suites against both, and records what happened in files that outlive any one report.

Postgres is one project, but a large one: about 1.8 million lines of C under `src` and `contrib`, a configure step that asks the compiler hundreds of questions, atomics, inline assembly, per function target attributes for CRC and popcount, and `sigsetjmp` under every error path. [rucc-real-corpus](https://github.com/tamnd/rucc-real-corpus) grades rucc on a ladder of whole projects. This repository does the same for one project that is too big to be a rung, with a harness shaped around it: per test records instead of per project ones, baselines per platform, and a map of what the tree asks of a compiler.

## The rules

**No compiler code.** Nothing here compiles C. Every compile goes to a real compiler, rucc or the reference, through a shim that only records. A fix that belongs in rucc goes to rucc.

**No Postgres patches.** The bytes that come out of the pinned tarball are the bytes the compiler sees. There is no patches directory, and a configuration file can hold only documented `configure` and meson options, which the loader checks. A build that needs an edit to get further is a finding against rucc, not a configuration.

**No copied Postgres files.** Postgres is downloaded, verified and unpacked into a cache outside the repository. Nothing from its tree is committed here, not a header, a test or an expected output.

**No reductions.** When rucc gets something wrong, cutting the failing file down to a small case is work for rucc's own test suites, and the case lives there. This repository only says which file, which test and which probe.

## Commands

The harness is one binary, `rpg`, plus the shim `rpg-cc` that has to sit next to it. It is run from anywhere inside the checkout and finds the root by walking up to `pins.toml`, or from `RPG_ROOT`.

```
cargo build --release
target/release/rpg fetch                       download REL_18_6, check its SHA-256 twice, unpack it
target/release/rpg build --cc gcc-16           configure and build with the minimal configuration, through the shim
target/release/rpg build --cc ~/rucc/target/release/rucc --system autoconf --level -O0
target/release/rpg test --suite regress        run the main regression suite against the only build in work/
target/release/rpg test --out work/DIR --row L64   the same for a named build, graded against the L64 baseline
target/release/rpg baseline --row L64          build with the row's reference compiler and run the suites three times
target/release/rpg demands                     scan the tree and rewrite demands.toml
target/release/rpg config-diff --a work/A --b work/B   compare what two configured trees decided, less config-divergences.toml
target/release/rpg repro --build work/DIR --file src/backend/parser/gram.c   bundle one translation unit for a bug report
target/release/rpg asm-audit                   list every inline assembly statement and rewrite asm-audit.toml
target/release/rpg frames --a work/A --b work/B   compare every function's stack frame between two builds
```

`rpg fetch` downloads with curl into `RPG_CACHE`, or `~/.cache/rpg`. The archive's SHA-256 has to match `pins.toml` every time it is used, including from the cache, and on a fetch it also has to match the `.sha256` file the Postgres project publishes next to the tarball, which catches a pin written down wrong.

`rpg build` takes `--cc`, `--level -O0` or `-O2` (the default), `--system meson` (the default) or `autoconf`, `--config minimal`, `--out DIR`, `--jobs N` and `--twice`. The build directory defaults to `work/<pin>-<config>-<system>-<level>-<compiler>` and holds `configure.log`, `build.log`, the Postgres build tree under `build/`, `compile.jsonl`, `compile_commands.json` and `build.json`, which says what was built with what, how long each step took, and where the build stopped if it stopped.

`rpg test` refuses to run as root, because initdb does. `--suite` is `regress`, the default, `isolation`, `ecpg`, `contrib`, `src/test/modules` as `modules`, or `world`. Under meson it runs `meson test --suite setup` and then the suite, for the first three. Under autoconf it runs `make check` for `regress` and `make -C <dir> check` for the others, with `-k` for `contrib` and `modules` so that one module failing does not stop the rest. Those two are a `pg_regress` run per module, which the log splits at the `# +++ regress check in contrib/amcheck +++` line Postgres's makefiles print ahead of each, and each test is named for its module, `amcheck/check_btree`, or `amcheck/isolation/read-write-unique` for an isolation run. TAP runs are left out of those two. `world` runs `make -k -j<jobs> -Otarget check-world PROVE_FLAGS=--timer` under autoconf, as upstream's CI does, and reads both kinds of run out of its log: each `pg_regress` test is named for its directory, `src/test/regress/boolean`, and each TAP script too, `src/bin/initdb/t/001_initdb.pl`, with the log of a failing script copied next to the diffs. ecpg's run prints no marker, so it is named for the directory make last entered. `PG_TEST_EXTRA` is passed through from the environment, and the `world` workflow sets it to what PG3's exit criterion names. It sets `PG_TEST_TIMEOUT_DEFAULT` from the row, doubled at `-O0`, copies `regression.out`, `regression.diffs` and the server logs of each run into `results/<suite>/run-<n>/`, and appends one line per test to `records.jsonl`. A failing test is `crashed` rather than `failed` when the postmaster log says a backend was terminated by a signal. It exits nonzero when a test fails that the baseline says passes, or any test fails when there is no baseline.

`rpg config-diff` reads `pg_config.h`, the variables `src/Makefile.global` sets and the probe answers from meson's log or configure's output in both trees, with each tree's own paths replaced so that two build directories compare equal. A difference listed in `config-divergences.toml`, or in the file `--divergences` names, is printed as explained along with the reason the entry gives, and an entry that matched nothing is printed so it can be taken out. Every entry has to say why the difference does not change which code is compiled. The command exits nonzero when any difference is left unexplained.

`rpg repro` finds the one call in a build's `compile.jsonl` that compiled the named file, which is given relative to the Postgres source tree, or to the build tree for a generated file such as `gram.c`. It writes a bundle directory, by default `repro/<path>` inside the build directory or `--out DIR`, with the preprocessed source `<name>.i`, made by running the recorded command again in the recorded directory with `-E -o` in place of `-c -o` and without the dependency file options, `command.txt` with the recorded command line, directory and environment, `compile.sh`, which compiles the `.i` again with the recorded flags minus `-I`, `-D`, `-U`, `-include`, `-M*` and the other preprocessor options, and `compiler.txt` with the compiler's `--version` and the commit of the checkout it was built in, since `rucc --version` names a release and not a commit. `CC=... compile.sh` runs the same compile with another compiler. When no call compiled the file it says so, and when several did, as for the files of `src/port` that meson builds three times, it lists their objects and `--object PART` picks one. This is what goes with a rucc bug report.

`rpg frames` compares how much stack each function takes in two builds. Postgres checks its own stack depth against `max_stack_depth` and has regression tests that recurse on purpose, so a compiler whose frames are a few times larger than gcc's fails them with `stack depth limit exceeded` while everything else passes. For each of `--a` and `--b` it runs every compile of a C file in `compile.jsonl` again in its recorded directory, with the recorded compiler or the one `--a-cc` or `--b-cc` names, adding `-fstack-usage`, dropping the dependency file options and writing the object to a scratch directory, so the build tree is not touched. It runs `--jobs` compiles at a time, by default one per core. Each function is keyed by the file compiled, the object and the name, so a static function with one name in two files is two entries, and a file compiled several times with different flags, as `src/common` and `src/port` are, is one entry per object. A function with no exact match on the other side is matched by the name before the first dot, which pairs gcc's clones such as `foo.isra.0` with a plain `foo`. The report, `frames/<a>-vs-<b>.md` or `--out FILE`, gives the number of functions matched and found on one side only, the median, 90th and 99th percentile and largest ratio of b over a, the totals and largest frame of each side, and the 50 functions with the largest ratio and the 50 with the largest frame in b, with dynamic frames flagged. It exits 0 either way, since it is a measurement and not a gate. The reports for `REL_18_6` with autoconf and the minimal configuration, rucc 0.11.19 against gcc 16 at `-O0` and `-O2`, are in `frames/`.

## The shim

`rpg build` copies `rpg-cc` into the build directory as `bin/cc` and `bin/gcc`, puts `bin` first on `PATH`, sets `CC` to it and writes `bin/rpg-cc.toml` naming the real compiler. The shim runs the real compiler with the same arguments and appends one JSON line per call to `compile.jsonl`: the argv, the working directory, the environment variables that change what a compiler does, the SHA-256 of every input and output, wall, user and system time and peak RSS of the child, the exit status or signal, and the first KiB of stderr. It prints nothing of its own, because configure judges a compiler partly by what it writes to stderr.

When the real compiler is rucc, which is anything whose `--version` starts with `rucc `, and rucc understands `-frucc-trace`, the shim adds `-frucc-trace=<tmp>` to every call that compiles C and folds the trace lines, with rucc's own phase and pass timings, into the record under `rucc`. Whether rucc understands the option is probed once per build by compiling a one line file, so an older rucc is simply run without it. `RPG_TWICE=1`, or `rpg build --twice`, compiles every translation unit a second time and fails the call if the two objects differ.

`RPG_REAL_CC`, `RPG_COMPILE_LOG`, `RPG_RUCC_TRACE` and `RPG_TWICE` override the settings file, which makes the shim usable by hand as well.

## Records

`records.jsonl` has one line per test per run, in the shape of rucc-real-corpus's records with the test as the unit:

```json
{"project":"postgres","pin":"REL_18_6","row":"L64","host":"server3","level":"-O2","system":"autoconf","config":"minimal","suite":"regress","test":"join_hash","outcome":"failed","class":"diff","seconds":4.1,"compiler":"rucc 0.12.3","rucc":"0.12.3","rucc-commit":"abc","reference":"gcc-16.2.0","baseline":"passed","artifacts":"runs/2026-10-14/L64-O2/regress/join_hash/"}
```

`outcome` is one of `passed`, `failed`, `crashed`, `timeout`, `build-failed` and `skipped`. `class` says how a failure failed: `diff`, `tap` or `error`. A run that did not finish, because pg_regress bailed out or meson timed it out, gets an extra line with the test `*`, so that a suite that stopped halfway cannot read as a suite with fewer tests.

## Rows and baselines

`rows.toml` has the four platforms rucc is graded on, each with its reference compiler: L64 (Linux x86-64, gcc-16), LA64 (Linux AArch64, gcc-16), M64 (macOS arm64, Apple clang) and W64 (Windows x86-64 under MSYS2 UCRT64, gcc). `provision/` has a script per row that installs what the minimal configuration and the TAP tests need. The Linux scripts create an unprivileged `pg` user and only touch `kernel.core_pattern` when asked with `--core-pattern`.

`rpg baseline --row L64` builds with the row's reference compiler and runs each suite three times, or `--runs N`. A test that passed every run is `passing`, one that failed every run is `failing`, and one that did both is `flaky`. The sets and the build and test times go to `baselines/<row>/<pin>/<config>/baseline.toml`, next to the records they came from.

## Demands

`demands.toml` is what the pinned tree asks of a C compiler beyond plain C11, found by a text scan of `src` and `contrib` with comments and literal contents removed: 128 bit integers, overflow, atomic and bit builtins, computed goto, inline assembly, target attributes, x86 and Arm intrinsics, cpuid, `sigsetjmp`, `PGDLLIMPORT` and so on, plus one entry per `__builtin_*` name, per attribute and per `pg_attribute_*` macro. Each entry lists the lines per file. The `rucc-status` and `rucc-issue` fields are for a person to fill in, and `rpg demands` keeps them when it rescans.

It counts what is written, not what one target compiles, and a feature behind a Postgres macro is counted where the macro is defined. It is a map for deciding what to look at, not a proof of what rucc needs.

## Inline assembly

`asm-audit.toml` lists every inline assembly statement under `src` and `contrib`, written by `rpg asm-audit` from a small scanner that skips comments and literals and splits each statement at its colons and commas. Each entry has the file, the line, the function or macro it is in, the `#if` lines around it, the qualifiers, the template, the outputs and inputs with their constraints, the clobbers and any goto labels. The top of the file counts statements per file, kind and qualifier, operands per constraint as written and per constraint letter, and clobbers, since those say what a compiler has to understand. Like `demands.toml` it covers every architecture Postgres supports, and the `guard` lines say which one a statement is for.

For REL_18_6 it finds 43 statements in 6 files: 35 extended, 6 basic (with no operands) and 2 in the MSVC `__asm` form. The output constraints are `=r`, `=&r`, `=q`, `=a`, `=m`, `=&b`, `+m`, `+q`, `+d` and `+R`, the input constraints are `r`, `m`, `i`, `a`, `d`, `rm` and the matching `0`, and the only clobbers are `memory` (33) and `cc` (20). The 16 that are not for another architecture, meaning x86-64 code and code for any architecture, use `=q`, `=m`, `=a`, `+q` and `+m` for outputs, `m`, `a`, `r`, `rm` and `0` for inputs, and `memory` and `cc`.

## Where it stands

PG0, the harness itself. The first runs were on server2, an Ubuntu 24.04 x86-64 machine with 6 shared cores, using gcc-16 16.0.1 (20260315) as the reference, meson and `-j4`:

- `rpg fetch` took 16 seconds, download included, and both hash checks passed.
- The gcc-16 build compiled 1577 translation units: 21 seconds to configure and 202 seconds to build on a quiet machine, 42 and 507 on a busy one, about 620 compiler user seconds either way. The largest compile is `gram.c`, at 218 MB peak RSS.
- The regression suite passed 231 of 231 tests, in 30 to 42 seconds including the temporary install. Three runs for the L64 baseline agreed, so nothing is flaky yet.
- Under meson, rucc 0.11.15 got through configure and compiled all 1073 translation units the backend needs, in 81 seconds and 196 user seconds, peaking at 304 MB on `gram.c`. It stopped at the link of `src/backend/postgres`, the one link meson does through a response file: rucc passed `-Wl,--as-needed` from `@postgres.rsp` to `ld` as it was, while the same option on a command line works.
- Under autoconf, which uses no response files, rucc built the whole of `world-bin`, 1417 translation units with no failures, in 25 seconds of configure and 108 of build. That build passed 231 of 231 regression tests in 45 seconds, and the version string in the `postgres` binary says `compiled by rucc 0.11.15`.
- `rpg config-diff` between the two meson trees shows rucc's configure answers differing in 9 `pg_config.h` defines. rucc answers no to the probes for `__get_cpuid`, `__get_cpuid_count`, the SSE 4.2, AVX-512 and XSAVE intrinsics and the `popcntq` inline assembly, so Postgres falls back to slicing by 8 CRC32C and no runtime popcount check. The other differences are the compiler version string and a few warning flags.

PG1, the same tree with the probes answered. On gpc, an x86-64 machine with 32 cores, using gcc-16 16.2.0 and rucc 0.11.18 at tamnd/rucc@9615dc28, autoconf and `-j32`:

- Both compilers built all 1423 translation units at `-O0` and at `-O2` with no failures. rucc's largest compile is `gram.c`, at 245 MB peak RSS at `-O0` and 301 MB at `-O2`, against gcc's 158 MB and 224 MB.
- `rpg config-diff` between the gcc-16 and rucc trees finds one difference at each level, `PG_VERSION_STR`, which `config-divergences.toml` explains. `pg_config.h`, `Makefile.global` and every configure answer are otherwise the same, including `__get_cpuid`, `__get_cpuid_count`, the SSE 4.2 and AVX-512 intrinsics and `popcntq`, so a rucc build now compiles the same CRC32C and popcount choosers gcc's does.

## Licence

Apache-2.0. See `LICENSE`.
