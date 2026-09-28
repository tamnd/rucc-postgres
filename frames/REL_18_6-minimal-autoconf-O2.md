# Stack frames, REL_18_6-minimal-autoconf-O2-gcc-16 against REL_18_6-minimal-autoconf-O2-rucc

Written by `rpg frames` on 2026-09-28 on GamingPC. Every compile of a C file in each build's `compile.jsonl` was run again in its recorded directory with `-fstack-usage`, and the frame sizes the compiler wrote were joined by source file, object and function. The ratio is b over a, so a ratio above 1 means b's frame is larger. A dynamic frame grows at run time by `alloca` or a variable length array, and its size here is only the fixed part.

| | a | b |
|---|---|---|
| build | REL_18_6-minimal-autoconf-O2-gcc-16 | REL_18_6-minimal-autoconf-O2-rucc |
| compiler | gcc-16 (GCC) 16.2.0 | rucc 0.11.19 |
| path | `/opt/gcc-16.2.0/bin/gcc-16` | `/home/gopher/pgwork/rc-main/target/release/rucc` |
| commit | not in a checkout | 771554bb3247aa90c72319ee3c5b72801e4a4362 |
| translation units | 1423 | 1423 |
| compiles that failed | 0 | 0 |
| functions | 21806 | 27026 |
| dynamic frames | 1584 | 0 |
| bytes, all functions | 3604120 | 4062360 |
| largest frame | `main` in src/bin/pg_waldump/pg_waldump.c, 114384 bytes | `main` in src/bin/pg_waldump/pg_waldump.c, 114704 bytes |
| `.su` lines that did not parse | 0 | 0 |
| functions named twice in one file | 2 | 0 |

## Matching

20532 functions are on both sides, 1274 only in a and 6494 only in b. 546 of the pairs were matched by the name before the first dot, which is how gcc names the clones it makes (`.isra.0`, `.constprop.0`, `.part.0`). A function on one side only is usually one the other compiler inlined everywhere it was called, or a static inline function from a header that one compiler emitted and the other did not.

Over the matched functions a takes 2962640 bytes and b takes 3908448, 1.32 times as much.

## Ratio

Over 20532 pairs:

| median | p90 | p99 | max |
|--:|--:|--:|--:|
| 1.00 | 2.00 | 6.00 | 2785.00 |

| b's frame | functions |
|---|--:|
| smaller in b | 1980 |
| the same | 9786 |
| up to 1.5 times | 3879 |
| 1.5 to 2 times | 3668 |
| 2 to 4 times | 854 |
| over 4 times | 365 |

## The 50 largest ratios

A function whose frame in a is 0 bytes and in b is not comes first.

| | function | file | a | b | b/a | flags |
|--:|---|---|--:|--:|--:|---|
| 1 | `select_default_timezone` | src/bin/initdb/findtimezone.c | 16 | 44560 | 2785.00 |  |
| 2 | `CheckPointBuffers` | src/backend/storage/buffer/bufmgr.c | 8 | 5232 | 654.00 |  |
| 3 | `pg_popcount_masked_avx512` | src/port/pg_popcount_avx512.c (pg_popcount_avx512.o) | 8 | 4928 | 616.00 |  |
| 4 | `pg_popcount_masked_avx512` | src/port/pg_popcount_avx512.c (pg_popcount_avx512_shlib.o) | 8 | 4928 | 616.00 |  |
| 5 | `pg_popcount_masked_avx512` | src/port/pg_popcount_avx512.c (pg_popcount_avx512_srv.o) | 8 | 4928 | 616.00 |  |
| 6 | `init_tablespaces` | src/bin/pg_upgrade/tablespace.c | 16 | 8416 | 526.00 |  |
| 7 | `pg_comp_crc32c_avx512` | src/port/pg_crc32c_sse42.c (pg_crc32c_sse42.o) | 8 | 4160 | 520.00 |  |
| 8 | `pg_comp_crc32c_avx512` | src/port/pg_crc32c_sse42.c (pg_crc32c_sse42_shlib.o) | 8 | 4160 | 520.00 |  |
| 9 | `pg_comp_crc32c_avx512` | src/port/pg_crc32c_sse42.c (pg_crc32c_sse42_srv.o) | 8 | 4160 | 520.00 |  |
| 10 | `XLogReadAhead` | src/backend/access/transam/xlogreader.c | 32 | 16528 | 516.50 |  |
| 11 | `XLogReadAhead` | src/bin/pg_rewind/xlogreader.c | 32 | 16528 | 516.50 |  |
| 12 | `XLogReadAhead` | src/bin/pg_waldump/xlogreader.c | 32 | 16528 | 516.50 |  |
| 13 | `main` | src/bin/pg_basebackup/pg_basebackup.c | 144 | 73568 | 510.89 |  |
| 14 | `PgArchiverMain` | src/backend/postmaster/pgarch.c | 16 | 8016 | 501.00 |  |
| 15 | `pg_popcount_avx512` | src/port/pg_popcount_avx512.c (pg_popcount_avx512.o) | 8 | 3264 | 408.00 |  |
| 16 | `pg_popcount_avx512` | src/port/pg_popcount_avx512.c (pg_popcount_avx512_shlib.o) | 8 | 3264 | 408.00 |  |
| 17 | `pg_popcount_avx512` | src/port/pg_popcount_avx512.c (pg_popcount_avx512_srv.o) | 8 | 3264 | 408.00 |  |
| 18 | `stop_postmaster` | src/test/regress/pg_regress.c | 8 | 2064 | 258.00 |  |
| 19 | `pg_tzset` | src/timezone/pgtz.c | 112 | 24112 | 215.29 |  |
| 20 | `ExecWindowAgg` | src/backend/executor/nodeWindowAgg.c | 32 | 6832 | 213.50 |  |
| 21 | `libpqrcv_exec` | src/backend/replication/libpqwalreceiver/libpqwalreceiver.c | 64 | 13408 | 209.50 |  |
| 22 | `pqsecure_raw_write` | src/interfaces/libpq/fe-secure.c | 8 | 1520 | 190.00 |  |
| 23 | `entryBeginPlaceToPage` | src/backend/access/gin/ginentrypage.c | 96 | 16528 | 172.17 | a dynamic,bounded |
| 24 | `_PrintTocData` | src/bin/pg_dump/pg_backup_tar.c | 48 | 8256 | 172.00 |  |
| 25 | `sql_help_TABLE` | src/bin/psql/sql_help.c | 8 | 1248 | 156.00 |  |
| 26 | `sql_help_WITH` | src/bin/psql/sql_help.c | 8 | 1248 | 156.00 |  |
| 27 | `create_target` | src/bin/pg_rewind/file_ops.c | 16 | 2096 | 131.00 |  |
| 28 | `remove_target` | src/bin/pg_rewind/file_ops.c | 16 | 2096 | 131.00 |  |
| 29 | `check_and_dump_old_cluster` | src/bin/pg_upgrade/check.c | 96 | 11632 | 121.17 |  |
| 30 | `main` | src/bin/pg_resetwal/pg_resetwal.c | 176 | 20480 | 116.36 |  |
| 31 | `spgPageIndexMultiDelete` | src/backend/access/spgist/spgdoinsert.c | 8 | 928 | 116.00 |  |
| 32 | `pg_get_wal_block_info` | contrib/pg_walinspect/pg_walinspect.c | 80 | 8784 | 109.80 |  |
| 33 | `StartupXLOG` | src/backend/access/transam/xlog.c | 192 | 20480 | 106.67 |  |
| 34 | `asyncQueueReadAllNotifications` | src/backend/commands/async.c | 80 | 8368 | 104.60 |  |
| 35 | `PreCommit_Notify` | src/backend/commands/async.c | 80 | 8256 | 103.20 |  |
| 36 | `_hash_expandtable` | src/backend/access/hash/hashpage.c | 208 | 20480 | 98.46 | a dynamic,bounded |
| 37 | `gistvacuumscan` | src/backend/access/gist/gistvacuum.c | 176 | 16768 | 95.27 | a dynamic,bounded |
| 38 | `BackendMain` | src/backend/tcop/backend_startup.c | 16 | 1312 | 82.00 |  |
| 39 | `heap_redo` | src/backend/access/heap/heapam_xlog.c | 112 | 8432 | 75.29 |  |
| 40 | `pg_utf8_verifystr` | src/common/wchar.c (wchar.o) | 16 | 1184 | 74.00 |  |
| 41 | `pg_utf8_verifystr` | src/common/wchar.c (wchar_srv.o) | 16 | 1184 | 74.00 |  |
| 42 | `get_db_rel_and_slot_infos` | src/bin/pg_upgrade/info.c | 128 | 8352 | 65.25 |  |
| 43 | `main` | src/bin/pg_rewind/pg_rewind.c | 96 | 5904 | 61.50 | a dynamic,bounded |
| 44 | `parseNodeString` | src/backend/nodes/readfuncs.c | 64 | 3808 | 59.50 |  |
| 45 | `main` | src/bin/pg_upgrade/pg_upgrade.c | 128 | 6496 | 50.75 | a dynamic,bounded |
| 46 | `expand_grouping_sets` | src/backend/parser/parse_agg.c | 8 | 400 | 50.00 |  |
| 47 | `dataBeginPlaceToPage` | src/backend/access/gin/gindatapage.c | 176 | 8576 | 48.73 | a dynamic,bounded |
| 48 | `check_new_cluster` | src/bin/pg_upgrade/check.c | 48 | 2304 | 48.00 |  |
| 49 | `pgstat_flush_backend` | src/backend/utils/activity/pgstat_backend.c | 64 | 3008 | 47.00 |  |
| 50 | `XLogInsert` | src/backend/access/transam/xloginsert.c | 192 | 8544 | 44.50 |  |

## The 50 largest frames in b

| | function | file | a | b | b/a | flags |
|--:|---|---|--:|--:|--:|---|
| 1 | `main` | src/bin/pg_waldump/pg_waldump.c | 114384 | 114704 | 1.00 | a dynamic,bounded |
| 2 | `GetWalStats` | contrib/pg_walinspect/pg_walinspect.c | 104832 | 104880 | 1.00 |  |
| 3 | `NIImportAffixes` | src/backend/tsearch/spell.c | 74064 | 74176 | 1.00 | a dynamic,bounded |
| 4 | `main` | src/bin/pg_basebackup/pg_basebackup.c | 144 | 73568 | 510.89 |  |
| 5 | `WriteBlockRefTable` | src/common/blkreftable.c (blkreftable.o) | 65680 | 65744 | 1.00 |  |
| 6 | `WriteBlockRefTable` | src/common/blkreftable.c (blkreftable_shlib.o) | 65680 | 65744 | 1.00 |  |
| 7 | `WriteBlockRefTable` | src/common/blkreftable.c (blkreftable_srv.o) | 65680 | 65744 | 1.00 |  |
| 8 | `heap_toast_insert_or_update` | src/backend/access/heap/heaptoast.c | 54592 | 54576 | 1.00 | a dynamic,bounded |
| 9 | `select_default_timezone` | src/bin/initdb/findtimezone.c | 16 | 44560 | 2785.00 |  |
| 10 | `pg_regexec` | src/backend/regex/regexec.c | 34752 | 34800 | 1.00 | a dynamic,bounded |
| 11 | `toast_build_flattened_tuple` | src/backend/access/heap/heaptoast.c | 26688 | 26688 | 1.00 |  |
| 12 | `scan_directory` | src/bin/pg_checksums/pg_checksums.c | 20496 | 24576 | 1.20 | a dynamic,bounded |
| 13 | `rewriteVisibilityMap` | src/bin/pg_upgrade/file.c | 24576 | 24576 | 1.00 |  |
| 14 | `pg_tzset` | src/timezone/pgtz.c | 112 | 24112 | 215.29 |  |
| 15 | `verify_heapam` | contrib/amcheck/verify_heapam.c | 20976 | 21104 | 1.01 | a dynamic,bounded |
| 16 | `_hash_expandtable` | src/backend/access/hash/hashpage.c | 208 | 20480 | 98.46 | a dynamic,bounded |
| 17 | `StartupXLOG` | src/backend/access/transam/xlog.c | 192 | 20480 | 106.67 |  |
| 18 | `RelationCopyStorageUsingBuffer` | src/backend/storage/buffer/bufmgr.c | 20496 | 20480 | 1.00 | a dynamic,bounded |
| 19 | `main` | src/bin/pg_resetwal/pg_resetwal.c | 176 | 20480 | 116.36 |  |
| 20 | `toast_flatten_tuple_to_datum` | src/backend/access/heap/heaptoast.c | 16800 | 16784 | 1.00 | a dynamic,bounded |
| 21 | `gistvacuumscan` | src/backend/access/gist/gistvacuum.c | 176 | 16768 | 95.27 | a dynamic,bounded |
| 22 | `toast_flatten_tuple` | src/backend/access/heap/heaptoast.c | 16704 | 16704 | 1.00 |  |
| 23 | `entryBeginPlaceToPage` | src/backend/access/gin/ginentrypage.c | 96 | 16528 | 172.17 | a dynamic,bounded |
| 24 | `XLogReadAhead` | src/backend/access/transam/xlogreader.c | 32 | 16528 | 516.50 |  |
| 25 | `XLogReadAhead` | src/bin/pg_rewind/xlogreader.c | 32 | 16528 | 516.50 |  |
| 26 | `XLogReadAhead` | src/bin/pg_waldump/xlogreader.c | 32 | 16528 | 516.50 |  |
| 27 | `dumpLOs` | src/bin/pg_dump/pg_dump.c | 16464 | 16480 | 1.00 |  |
| 28 | `AddToDataDirLockFile` | src/backend/utils/init/miscinit.c | 16464 | 16464 | 1.00 |  |
| 29 | `ltsWriteBlock` | src/backend/utils/sort/logtape.c | 12288 | 16384 | 1.33 |  |
| 30 | `local_queue_fetch_file` | src/bin/pg_rewind/local_source.c | 16384 | 16384 | 1.00 |  |
| 31 | `local_queue_fetch_range` | src/bin/pg_rewind/local_source.c | 16384 | 16384 | 1.00 |  |
| 32 | `search_directory` | src/bin/pg_waldump/pg_waldump.c | 12288 | 16384 | 1.33 |  |
| 33 | `regression_main` | src/test/regress/pg_regress.c | 10992 | 14528 | 1.32 | a dynamic,bounded |
| 34 | `ExtractReplicaIdentity` | src/backend/access/heap/heapam.c | 14464 | 14480 | 1.00 |  |
| 35 | `heap_toast_delete` | src/backend/access/heap/heaptoast.c | 14432 | 14448 | 1.00 |  |
| 36 | `libpqrcv_exec` | src/backend/replication/libpqwalreceiver/libpqwalreceiver.c | 64 | 13408 | 209.50 |  |
| 37 | `base_yyparse` | src/interfaces/ecpg/preproc/preproc.c | 12016 | 12384 | 1.03 | a dynamic,bounded |
| 38 | `read_into_scalar_list` | src/pl/plpgsql/src/pl_gram.c | 12352 | 12368 | 1.00 |  |
| 39 | `tar_write_padding_data` | src/bin/pg_basebackup/walmethods.c | 12288 | 12288 | 1.00 |  |
| 40 | `ginbulkdelete` | src/backend/access/gin/ginvacuum.c | 11968 | 12000 | 1.00 | a dynamic,bounded |
| 41 | `_hash_squeezebucket` | src/backend/access/hash/hashovfl.c | 11664 | 11744 | 1.01 | a dynamic,bounded |
| 42 | `check_and_dump_old_cluster` | src/bin/pg_upgrade/check.c | 96 | 11632 | 121.17 |  |
| 43 | `gin_check_parent_keys_consistency` | contrib/amcheck/verify_gin.c | 10928 | 11088 | 1.01 | a dynamic,bounded |
| 44 | `gingetbitmap` | src/backend/access/gin/ginget.c | 480 | 10976 | 22.87 |  |
| 45 | `basic_archive_file` | contrib/basic_archive/basic_archive.c | 9440 | 10800 | 1.14 | a dynamic,bounded |
| 46 | `exec_replication_command` | src/backend/replication/walsender.c | 320 | 10736 | 33.55 | a dynamic,bounded |
| 47 | `writeTimeLineHistory` | src/backend/access/transam/timeline.c | 10400 | 10448 | 1.00 | a dynamic,bounded |
| 48 | `ginbuild` | src/backend/access/gin/gininsert.c | 10080 | 10176 | 1.01 | a dynamic,bounded |
| 49 | `blbuild` | contrib/bloom/blinsert.c | 10128 | 10160 | 1.00 | a dynamic,bounded |
| 50 | `ClientAuthentication` | src/backend/libpq/auth.c | 4032 | 10000 | 2.48 | a dynamic,bounded |

## The largest frames only in a

| | function | file | bytes | qualifier |
|--:|---|---|--:|---|
| 1 | `BaseBackup` | src/bin/pg_basebackup/pg_basebackup.c | 66896 | dynamic,bounded |
| 2 | `identify_system_timezone` | src/bin/initdb/findtimezone.c | 43488 | static |
| 3 | `_tarAddFile` | src/bin/pg_dump/pg_backup_tar.c | 33360 | dynamic,bounded |
| 4 | `pg_tzset.part.0` | src/timezone/pgtz.c | 24000 | static |
| 5 | `XLogDecodeNextRecord` | src/backend/access/transam/xlogreader.c | 16512 | static |
| 6 | `XLogDecodeNextRecord` | src/bin/pg_rewind/xlogreader.c | 16512 | static |
| 7 | `XLogDecodeNextRecord` | src/bin/pg_waldump/xlogreader.c | 16512 | static |
| 8 | `entrySplitPage.isra` | src/backend/access/gin/ginentrypage.c | 16496 | static |
| 9 | `XLogFileCopy` | src/backend/access/transam/xlog.c | 16384 | static |
| 10 | `WriteEmptyXLOG` | src/bin/pg_resetwal/pg_resetwal.c | 16384 | static |
| 11 | `libpqrcv_processTuples.constprop` | src/backend/replication/libpqwalreceiver/libpqwalreceiver.c | 13424 | static |
| 12 | `gistvacuum_delete_empty_pages` | src/backend/access/gist/gistvacuum.c | 12464 | static |
| 13 | `_hash_alloc_buckets` | src/backend/access/hash/hashpage.c | 12288 | static |
| 14 | `collectMatchesForHeapRow` | src/backend/access/gin/ginget.c | 10432 | dynamic,bounded |
| 15 | `SendTimeLineHistory` | src/backend/replication/walsender.c | 9344 | static |
| 16 | `GetWALBlockInfo` | contrib/pg_walinspect/pg_walinspect.c | 8592 | static |
| 17 | `get_tablespace_paths` | src/bin/pg_upgrade/tablespace.c | 8400 | static |
| 18 | `dataSplitPageInternal.isra` | src/backend/access/gin/gindatapage.c | 8320 | static |
| 19 | `heap_xlog_multi_insert` | src/backend/access/heap/heapam_xlog.c | 8304 | static |
| 20 | `asyncQueueProcessPageEntries.isra` | src/backend/commands/async.c | 8304 | static |

## The largest frames only in b

| | function | file | bytes | qualifier |
|--:|---|---|--:|---|
| 1 | `tarClose` | src/bin/pg_dump/pg_backup_tar.c | 33376 | static |
| 2 | `lazy_vacuum` | src/backend/access/heap/vacuumlazy.c | 5168 | static |
| 3 | `CloneForeignKeyConstraints` | src/backend/commands/tablecmds.c | 2144 | static |
| 4 | `exec_command` | src/bin/psql/command.c | 1376 | static |
| 5 | `pg_lfind32_simd_helper` | src/backend/storage/ipc/procarray.c | 1232 | static |
| 6 | `pg_lfind32_simd_helper` | src/backend/storage/lmgr/predicate.c | 1232 | static |
| 7 | `pg_lfind32_simd_helper` | src/backend/utils/time/snapmgr.c | 1232 | static |
| 8 | `FileSetPath` | src/backend/storage/file/fileset.c | 1072 | static |
| 9 | `MergeAttributes` | src/backend/commands/tablecmds.c | 608 | static |
| 10 | `local_ts_node_16_get_insertpos` | src/backend/access/common/tidstore.c | 592 | static |
| 11 | `shared_ts_node_16_get_insertpos` | src/backend/access/common/tidstore.c | 592 | static |
| 12 | `local_ts_node_search` | src/backend/access/common/tidstore.c | 528 | static |
| 13 | `shared_ts_node_search` | src/backend/access/common/tidstore.c | 528 | static |
| 14 | `consider_groupingsets_paths` | src/backend/optimizer/plan/planner.c | 432 | static |
| 15 | `ExtendBufferedRelShared` | src/backend/storage/buffer/bufmgr.c | 400 | static |
| 16 | `ExecMerge` | src/backend/executor/nodeModifyTable.c | 352 | static |
| 17 | `heap_lock_updated_tuple` | src/backend/access/heap/heapam.c | 336 | static |
| 18 | `jsonb_from_cstring` | src/backend/utils/adt/jsonb.c | 304 | static |
| 19 | `bt_target_page_check` | contrib/amcheck/verify_nbtree.c | 288 | static |
| 20 | `distribute_quals_to_rels` | src/backend/optimizer/plan/initsplan.c | 288 | static |
