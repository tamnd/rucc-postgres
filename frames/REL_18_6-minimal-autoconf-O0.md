# Stack frames, REL_18_6-minimal-autoconf-O0-gcc-16 against REL_18_6-minimal-autoconf-O0-rucc

Written by `rpg frames` on 2026-09-28 on GamingPC. Every compile of a C file in each build's `compile.jsonl` was run again in its recorded directory with `-fstack-usage`, and the frame sizes the compiler wrote were joined by source file, object and function. The ratio is b over a, so a ratio above 1 means b's frame is larger. A dynamic frame grows at run time by `alloca` or a variable length array, and its size here is only the fixed part.

| | a | b |
|---|---|---|
| build | REL_18_6-minimal-autoconf-O0-gcc-16 | REL_18_6-minimal-autoconf-O0-rucc |
| compiler | gcc-16 (GCC) 16.2.0 | rucc 0.11.19 |
| path | `/opt/gcc-16.2.0/bin/gcc-16` | `/home/gopher/pgwork/rc-main/target/release/rucc` |
| commit | not in a checkout | 771554bb3247aa90c72319ee3c5b72801e4a4362 |
| translation units | 1423 | 1423 |
| compiles that failed | 0 | 0 |
| functions | 37203 | 37204 |
| dynamic frames | 1904 | 0 |
| bytes, all functions | 4820400 | 4554024 |
| largest frame | `main` in src/bin/pg_waldump/pg_waldump.c, 105056 bytes | `main` in src/bin/pg_waldump/pg_waldump.c, 105104 bytes |

## Matching

37202 functions are on both sides, 1 only in a and 2 only in b. 0 of the pairs were matched by the name before the first dot, which is how gcc names the clones it makes (`.isra.0`, `.constprop.0`, `.part.0`). A function on one side only is usually one the other compiler inlined everywhere it was called, or a static inline function from a header that one compiler emitted and the other did not.

Over the matched functions a takes 4820320 bytes and b takes 4553976, 0.94 times as much.

## Ratio

Over 37202 pairs:

| median | p90 | p99 | max |
|--:|--:|--:|--:|
| 1.00 | 1.01 | 2.00 | 16.00 |

| b's frame | functions |
|---|--:|
| smaller in b | 15702 |
| the same | 17676 |
| up to 1.5 times | 3130 |
| 1.5 to 2 times | 503 |
| 2 to 4 times | 155 |
| over 4 times | 36 |

## The 50 largest ratios

A function whose frame in a is 0 bytes and in b is not comes first.

| | function | file | a | b | b/a | flags |
|--:|---|---|--:|--:|--:|---|
| 1 | `vector8_min` | src/backend/access/common/tidstore.c | 16 | 256 | 16.00 |  |
| 2 | `vector8_ssub` | src/backend/utils/adt/json.c | 16 | 256 | 16.00 |  |
| 3 | `vector8_ssub` | src/common/jsonapi.c (jsonapi.o) | 16 | 256 | 16.00 |  |
| 4 | `vector8_ssub` | src/common/jsonapi.c (jsonapi_shlib.o) | 16 | 256 | 16.00 |  |
| 5 | `vector8_ssub` | src/common/jsonapi.c (jsonapi_srv.o) | 16 | 256 | 16.00 |  |
| 6 | `vector8_eq` | src/backend/access/common/tidstore.c | 16 | 240 | 15.00 |  |
| 7 | `vector32_eq` | src/backend/storage/ipc/procarray.c | 16 | 240 | 15.00 |  |
| 8 | `vector32_eq` | src/backend/storage/lmgr/predicate.c | 16 | 240 | 15.00 |  |
| 9 | `vector8_eq` | src/backend/utils/adt/json.c | 16 | 240 | 15.00 |  |
| 10 | `vector32_eq` | src/backend/utils/time/snapmgr.c | 16 | 240 | 15.00 |  |
| 11 | `vector8_eq` | src/common/jsonapi.c (jsonapi.o) | 16 | 240 | 15.00 |  |
| 12 | `vector8_eq` | src/common/jsonapi.c (jsonapi_shlib.o) | 16 | 240 | 15.00 |  |
| 13 | `vector8_eq` | src/common/jsonapi.c (jsonapi_srv.o) | 16 | 240 | 15.00 |  |
| 14 | `vector8_eq` | src/common/wchar.c (wchar.o) | 16 | 240 | 15.00 |  |
| 15 | `vector8_eq` | src/common/wchar.c (wchar_shlib.o) | 16 | 240 | 15.00 |  |
| 16 | `vector8_eq` | src/common/wchar.c (wchar_srv.o) | 16 | 240 | 15.00 |  |
| 17 | `vector32_or` | src/backend/storage/ipc/procarray.c | 16 | 208 | 13.00 |  |
| 18 | `vector32_or` | src/backend/storage/lmgr/predicate.c | 16 | 208 | 13.00 |  |
| 19 | `vector32_or` | src/backend/utils/time/snapmgr.c | 16 | 208 | 13.00 |  |
| 20 | `vector8_or` | src/common/wchar.c (wchar.o) | 16 | 208 | 13.00 |  |
| 21 | `vector8_or` | src/common/wchar.c (wchar_shlib.o) | 16 | 208 | 13.00 |  |
| 22 | `vector8_or` | src/common/wchar.c (wchar_srv.o) | 16 | 208 | 13.00 |  |
| 23 | `ATExecCmd` | src/backend/commands/tablecmds.c | 128 | 848 | 6.62 | a dynamic,bounded |
| 24 | `vector8_highbit_mask` | src/backend/access/common/tidstore.c | 16 | 80 | 5.00 |  |
| 25 | `LagTrackerRead` | src/backend/replication/walsender.c | 16 | 80 | 5.00 |  |
| 26 | `vector8_is_highbit_set` | src/backend/storage/ipc/procarray.c | 16 | 80 | 5.00 |  |
| 27 | `vector8_is_highbit_set` | src/backend/storage/lmgr/predicate.c | 16 | 80 | 5.00 |  |
| 28 | `vector8_is_highbit_set` | src/backend/utils/adt/json.c | 16 | 80 | 5.00 |  |
| 29 | `vector8_is_highbit_set` | src/backend/utils/time/snapmgr.c | 16 | 80 | 5.00 |  |
| 30 | `vector8_is_highbit_set` | src/common/jsonapi.c (jsonapi.o) | 16 | 80 | 5.00 |  |
| 31 | `vector8_is_highbit_set` | src/common/jsonapi.c (jsonapi_shlib.o) | 16 | 80 | 5.00 |  |
| 32 | `vector8_is_highbit_set` | src/common/jsonapi.c (jsonapi_srv.o) | 16 | 80 | 5.00 |  |
| 33 | `vector8_is_highbit_set` | src/common/wchar.c (wchar.o) | 16 | 80 | 5.00 |  |
| 34 | `vector8_is_highbit_set` | src/common/wchar.c (wchar_shlib.o) | 16 | 80 | 5.00 |  |
| 35 | `vector8_is_highbit_set` | src/common/wchar.c (wchar_srv.o) | 16 | 80 | 5.00 |  |
| 36 | `GlobalVisUpdateApply` | src/backend/storage/ipc/procarray.c | 32 | 144 | 4.50 |  |
| 37 | `uuid_2_double` | contrib/btree_gist/btree_uuid.c | 16 | 64 | 4.00 |  |
| 38 | `vector8_broadcast` | src/backend/access/common/tidstore.c | 16 | 64 | 4.00 |  |
| 39 | `toast_get_compression_id` | src/backend/access/common/toast_compression.c | 16 | 64 | 4.00 |  |
| 40 | `remove_gene` | src/backend/optimizer/geqo/geqo_erx.c | 16 | 64 | 4.00 |  |
| 41 | `vector32_broadcast` | src/backend/storage/ipc/procarray.c | 16 | 64 | 4.00 |  |
| 42 | `vector32_broadcast` | src/backend/storage/lmgr/predicate.c | 16 | 64 | 4.00 |  |
| 43 | `date2timestamp_no_overflow` | src/backend/utils/adt/date.c | 16 | 64 | 4.00 |  |
| 44 | `vector8_broadcast` | src/backend/utils/adt/json.c | 16 | 64 | 4.00 |  |
| 45 | `find_param_generator_initplan` | src/backend/utils/adt/ruleutils.c | 16 | 64 | 4.00 |  |
| 46 | `vector32_broadcast` | src/backend/utils/time/snapmgr.c | 16 | 64 | 4.00 |  |
| 47 | `create_rel_filename_map` | src/bin/pg_upgrade/info.c | 16 | 64 | 4.00 |  |
| 48 | `vector8_broadcast` | src/common/jsonapi.c (jsonapi.o) | 16 | 64 | 4.00 |  |
| 49 | `vector8_broadcast` | src/common/jsonapi.c (jsonapi_shlib.o) | 16 | 64 | 4.00 |  |
| 50 | `vector8_broadcast` | src/common/jsonapi.c (jsonapi_srv.o) | 16 | 64 | 4.00 |  |

## The 50 largest frames in b

| | function | file | a | b | b/a | flags |
|--:|---|---|--:|--:|--:|---|
| 1 | `main` | src/bin/pg_waldump/pg_waldump.c | 105056 | 105104 | 1.00 | a dynamic,bounded |
| 2 | `GetWalStats` | contrib/pg_walinspect/pg_walinspect.c | 104624 | 104640 | 1.00 |  |
| 3 | `BaseBackup` | src/bin/pg_basebackup/pg_basebackup.c | 66976 | 68992 | 1.03 | a dynamic,bounded |
| 4 | `WriteBlockRefTable` | src/common/blkreftable.c (blkreftable.o) | 65696 | 65680 | 1.00 |  |
| 5 | `WriteBlockRefTable` | src/common/blkreftable.c (blkreftable_shlib.o) | 65696 | 65680 | 1.00 |  |
| 6 | `WriteBlockRefTable` | src/common/blkreftable.c (blkreftable_srv.o) | 65696 | 65680 | 1.00 |  |
| 7 | `heap_toast_insert_or_update` | src/backend/access/heap/heaptoast.c | 54624 | 54592 | 1.00 | a dynamic,bounded |
| 8 | `identify_system_timezone` | src/bin/initdb/findtimezone.c | 43472 | 43520 | 1.00 |  |
| 9 | `NIImportOOAffixes` | src/backend/tsearch/spell.c | 41200 | 41216 | 1.00 | a dynamic,bounded |
| 10 | `pg_regexec` | src/backend/regex/regexec.c | 34720 | 34720 | 1.00 |  |
| 11 | `NIImportAffixes` | src/backend/tsearch/spell.c | 32928 | 33008 | 1.00 | a dynamic,bounded |
| 12 | `_tarAddFile` | src/bin/pg_dump/pg_backup_tar.c | 32864 | 32848 | 1.00 |  |
| 13 | `toast_build_flattened_tuple` | src/backend/access/heap/heaptoast.c | 26704 | 26720 | 1.00 |  |
| 14 | `rewriteVisibilityMap` | src/bin/pg_upgrade/file.c | 28672 | 24576 | 0.86 |  |
| 15 | `pg_tzset` | src/timezone/pgtz.c | 24000 | 24016 | 1.00 |  |
| 16 | `verify_heapam` | contrib/amcheck/verify_heapam.c | 21008 | 20928 | 1.00 | a dynamic,bounded |
| 17 | `RelationCopyStorageUsingBuffer` | src/backend/storage/buffer/bufmgr.c | 24592 | 20480 | 0.83 | a dynamic,bounded |
| 18 | `scan_file` | src/bin/pg_checksums/pg_checksums.c | 24592 | 20480 | 0.83 | a dynamic,bounded |
| 19 | `toast_flatten_tuple_to_datum` | src/backend/access/heap/heaptoast.c | 16768 | 16784 | 1.00 | a dynamic,bounded |
| 20 | `toast_flatten_tuple` | src/backend/access/heap/heaptoast.c | 16704 | 16720 | 1.00 |  |
| 21 | `entrySplitPage` | src/backend/access/gin/ginentrypage.c | 16544 | 16544 | 1.00 |  |
| 22 | `XLogDecodeNextRecord` | src/backend/access/transam/xlogreader.c | 16512 | 16544 | 1.00 |  |
| 23 | `XLogDecodeNextRecord` | src/bin/pg_rewind/xlogreader.c | 16512 | 16544 | 1.00 |  |
| 24 | `XLogDecodeNextRecord` | src/bin/pg_waldump/xlogreader.c | 16512 | 16544 | 1.00 |  |
| 25 | `dumpLOs` | src/bin/pg_dump/pg_dump.c | 16464 | 16480 | 1.00 |  |
| 26 | `AddToDataDirLockFile` | src/backend/utils/init/miscinit.c | 16464 | 16464 | 1.00 |  |
| 27 | `XLogFileCopy` | src/backend/access/transam/xlog.c | 20480 | 16384 | 0.80 |  |
| 28 | `WriteEmptyXLOG` | src/bin/pg_resetwal/pg_resetwal.c | 20480 | 16384 | 0.80 |  |
| 29 | `local_queue_fetch_file` | src/bin/pg_rewind/local_source.c | 20480 | 16384 | 0.80 |  |
| 30 | `local_queue_fetch_range` | src/bin/pg_rewind/local_source.c | 20480 | 16384 | 0.80 |  |
| 31 | `search_directory` | src/bin/pg_waldump/pg_waldump.c | 20480 | 16384 | 0.80 |  |
| 32 | `ExtractReplicaIdentity` | src/backend/access/heap/heapam.c | 14496 | 14480 | 1.00 |  |
| 33 | `heap_toast_delete` | src/backend/access/heap/heaptoast.c | 14464 | 14448 | 1.00 |  |
| 34 | `libpqrcv_processTuples` | src/backend/replication/libpqwalreceiver/libpqwalreceiver.c | 13424 | 13408 | 1.00 |  |
| 35 | `gistvacuum_delete_empty_pages` | src/backend/access/gist/gistvacuum.c | 12416 | 12432 | 1.00 |  |
| 36 | `base_yyparse` | src/interfaces/ecpg/preproc/preproc.c | 12656 | 12400 | 0.98 | a dynamic,bounded |
| 37 | `read_into_scalar_list` | src/pl/plpgsql/src/pl_gram.c | 12368 | 12384 | 1.00 |  |
| 38 | `_hash_alloc_buckets` | src/backend/access/hash/hashpage.c | 20480 | 12288 | 0.60 |  |
| 39 | `ltsWriteBlock` | src/backend/utils/sort/logtape.c | 20480 | 12288 | 0.60 |  |
| 40 | `tar_write_padding_data` | src/bin/pg_basebackup/walmethods.c | 20480 | 12288 | 0.60 |  |
| 41 | `ginbulkdelete` | src/backend/access/gin/ginvacuum.c | 11872 | 11856 | 1.00 |  |
| 42 | `_hash_squeezebucket` | src/backend/access/hash/hashovfl.c | 11664 | 11744 | 1.01 | a dynamic,bounded |
| 43 | `writeTimeLineHistory` | src/backend/access/transam/timeline.c | 10416 | 10432 | 1.00 | a dynamic,bounded |
| 44 | `collectMatchesForHeapRow` | src/backend/access/gin/ginget.c | 10400 | 10416 | 1.00 | a dynamic,bounded |
| 45 | `blbuild` | contrib/bloom/blinsert.c | 10112 | 10112 | 1.00 | a dynamic,bounded |
| 46 | `ginbuild` | src/backend/access/gin/gininsert.c | 10000 | 10016 | 1.00 | a dynamic,bounded |
| 47 | `blbulkdelete` | contrib/bloom/blvacuum.c | 9984 | 9984 | 1.00 |  |
| 48 | `_gin_parallel_build_main` | src/backend/access/gin/gininsert.c | 9968 | 9952 | 1.00 | a dynamic,bounded |
| 49 | `gin_check_parent_keys_consistency` | contrib/amcheck/verify_gin.c | 9984 | 9888 | 0.99 | a dynamic,bounded |
| 50 | `ginvacuumcleanup` | src/backend/access/gin/ginvacuum.c | 9760 | 9792 | 1.00 |  |

## The largest frames only in a

| | function | file | bytes | qualifier |
|--:|---|---|--:|---|
| 1 | `check_table` | contrib/isn/isn.c | 80 | static |

## The largest frames only in b

| | function | file | bytes | qualifier |
|--:|---|---|--:|---|
| 1 | `ItemPointerSetInvalid` | src/backend/commands/constraint.c | 32 | static |
| 2 | `BlockIdSet` | src/backend/commands/constraint.c | 16 | static |
