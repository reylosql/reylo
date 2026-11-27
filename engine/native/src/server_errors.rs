//! Whether a database's error is the statement's own answer: one the server raises because of what
//! the statement says, read against the schema and the session's settings, so that the same
//! statement over the same schema raises it again. Only such an error is the statement's. A host
//! answers a describe with it, the engine reads it as information (a probe that does not prepare, a
//! diagnostic), and the analysis it ends in may be kept in `reylo/.cache/` until the schema moves.
//!
//! Every other error says something of the moment rather than of the statement: another session's
//! lock or a deadlock, a cancel, a limit on time, memory or connections, the network, a replica, the
//! server's own fault. A host fails the analysis with it, so nothing is kept and the next analysis
//! asks again, and a cache writer keeps no analysis that carries one. The errors are listed by what
//! they are allowed to be, never by what they are denied: a code this module does not know, such as
//! one a newer server adds, is not the statement's.

use reylo_engine_core::dialect::Dialect;

/// Whether an error with the database's own `code` (Postgres's SQLSTATE, MySQL's or ClickHouse's
/// error number, SQLite's result code's name) is the statement's, for a reader that has the code
/// alone, as a cache writer reading an analysis's diagnostics has.
pub fn of_statement(dialect: Dialect, code: &str) -> bool {
    match dialect {
        Dialect::Postgres => postgres(code),
        Dialect::Mysql => code.parse().is_ok_and(|number| mysql(number, None, None)),
        Dialect::Clickhouse => code.parse().is_ok_and(clickhouse),
        Dialect::Sqlite => sqlite(code),
    }
}

/// Whether an analysis whose diagnostic carries `code` may be kept in `reylo/.cache/`: an error
/// that is the statement's ([`of_statement`]) and that no privilege decides. A `GRANT` moves no
/// schema's fingerprint, so a statement refused for a privilege the role lacked would stay refused
/// after the grant. The engine still reads such an error as the statement's answer.
pub fn kept(dialect: Dialect, code: &str) -> bool {
    of_statement(dialect, code)
        && match dialect {
            Dialect::Postgres => code != "42501",
            Dialect::Mysql => code.parse().is_ok_and(|n: u16| MYSQL_PRIVILEGE.binary_search_by_key(&n, |&(n, _)| n).is_err()),
            Dialect::Clickhouse => code.parse().is_ok_and(|n: u32| CLICKHOUSE_PRIVILEGE.binary_search_by_key(&n, |&(n, _)| n).is_err()),
            Dialect::Sqlite => true,
        }
}

/// Postgres: by the SQLSTATE's class, its first two characters. A describe is parse analysis, which
/// reads the statement against the catalog and raises these: 0A (a feature not supported), 22 (a
/// literal that does not read as its type, as `'x'::int4`), 3F (a schema that does not exist), 42
/// (syntax, a name not found, a type that does not fit, a privilege) and 54 (a program limit: too
/// many columns, too deep). The other classes are the session's or the server's: 08 the connection,
/// 25 the transaction, 40 a deadlock or a serialization failure, 53 memory, disk or connections, 55 a
/// lock or an object in use, 57 a cancel or a shutdown, 58 the system, and XX an internal error, as a
/// relation dropped while the describe read it raises. In class 42, 42P05 is the session's too: a
/// statement name already prepared, as a transaction pooler that shares server sessions leaves one.
/// In class 55, 55000 is the statement's: the rewriter raises it as it reads a write to a view that
/// cannot be updated. So is 26000, which the describe of an `EXECUTE` raises for a name the session
/// never prepared (a host tells one of its own statements' names apart, as a pooler's).
pub fn postgres(sqlstate: &str) -> bool {
    matches!(sqlstate, "55000" | "26000") || (sqlstate.len() == 5 && matches!(sqlstate.get(..2), Some("0A" | "22" | "3F" | "42" | "54")) && sqlstate != "42P05")
}

/// MySQL: by the error's number, since its SQLSTATE says too little (HY000 holds `ER_UNSUPPORTED_PS`,
/// a statement the server does not prepare, beside `ER_LOCK_WAIT_TIMEOUT`, and 42000 holds
/// `ER_MAX_PREPARED_STMT_COUNT_REACHED`, a limit every session's prepared statements count toward). A
/// number MySQL 8.4 sends a client is the statement's unless [`MYSQL_SERVER`] lists it, or it is
/// `ER_UNKNOWN_ERROR` with a `message` of Vitess's about a tablet's or the cluster's state
/// ([`vitess_state`]). A number MySQL 8.4 does not send, as a newer server's, is the statement's
/// only where the server's `sqlstate` is of a class a statement's error has: 42 (syntax and access),
/// 22 (data), 21 (a row's degree), 0A (not supported) or 3D (no database). Without the SQLSTATE such
/// a number is not the statement's.
pub fn mysql(number: u16, sqlstate: Option<&str>, message: Option<&str>) -> bool {
    if known(MYSQL_NUMBERS, number) {
        return MYSQL_SERVER.binary_search_by_key(&number, |&(n, _)| n).is_err() && !(number == UNKNOWN_ERROR && message.is_some_and(vitess_state));
    }
    sqlstate.is_some_and(|s| s.len() == 5 && matches!(s.get(..2), Some("42" | "22" | "21" | "0A" | "3D")))
}

/// MySQL's `ER_UNKNOWN_ERROR`, which Vitess answers with both for a statement it cannot plan and for
/// most of its own failures.
const UNKNOWN_ERROR: u16 = 1105;

/// Whether a Vitess message is about a tablet's or the cluster's state rather than the statement:
/// no healthy tablet, a tablet not serving or shutting down, a reparent or a reshard in progress, by
/// the words Vitess writes them in (`go/vt/vtgate/tabletgateway.go`, `go/vt/vterrors/constants.go`,
/// `go/vt/vtgate/buffer/buffer.go`), or a tablet a gRPC call found unavailable.
pub fn vitess_state(message: &str) -> bool {
    ["no healthy tablet available", "found no available tablet", "operation not allowed in state", "there may be a reparent operation in progress", "current keyspace is being resharded", "tx engine can't accept new connections", "code = Unavailable"].iter().any(|words| message.contains(words))
}

/// ClickHouse: by the error's number, which carries no class of its own. A number ClickHouse 26.10
/// defines is the statement's unless [`CLICKHOUSE_SERVER`] lists it; one it does not define is not.
pub fn clickhouse(number: u32) -> bool {
    known(CLICKHOUSE_NUMBERS, number) && CLICKHOUSE_SERVER.binary_search_by_key(&number, |&(n, _)| n).is_err()
}

/// SQLite: by the result code's name. Compiling a statement fails with `SQLITE_ERROR` (syntax, a name
/// not found) or its `SQLITE_ERROR_MISSING_COLLSEQ` (a collation not defined), `SQLITE_AUTH` (an
/// authorizer that refuses it, as Cloudflare D1's refuses a function it does not allow) or
/// `SQLITE_TOOBIG` (longer than the connection allows). The rest are the connection's or the file's:
/// `SQLITE_BUSY` and `SQLITE_LOCKED` another connection's lock, `SQLITE_NOMEM`, `SQLITE_IOERR`,
/// `SQLITE_CORRUPT`, `SQLITE_CANTOPEN`, `SQLITE_SCHEMA`, a schema that moved while it compiled, and
/// `SQLITE_AUTH_USER`, a sign-in.
pub fn sqlite(code: &str) -> bool {
    matches!(code, "SQLITE_ERROR" | "SQLITE_ERROR_MISSING_COLLSEQ" | "SQLITE_AUTH" | "SQLITE_TOOBIG")
}

/// Whether `number` falls in one of `runs`, each `(first, last)`, sorted.
fn known<N: Ord + Copy>(runs: &[(N, N)], number: N) -> bool {
    let at = runs.partition_point(|&(_, last)| last < number);
    runs.get(at).is_some_and(|&(first, _)| first <= number)
}

/// Every number MySQL 8.4 sends a client, as runs of consecutive numbers, `(first, last)`: the run
/// time's list (src/error-codes-mysql.ts), which a test holds this one to.
const MYSQL_NUMBERS: &[(u16, u16)] = &[(1002, 1008), (1010, 1010), (1012, 1013), (1015, 1018), (1020, 1020), (1022, 1022), (1024, 1027), (1030, 1038), (1040, 1076), (1079, 1149), (1152, 1164), (1166, 1173), (1175, 1182), (1184, 1184), (1188, 1197), (1199, 1207), (1210, 1211), (1213, 1218), (1220, 1244), (1246, 1253), (1256, 1267), (1269, 1274), (1276, 1281), (1283, 1292), (1294, 1345), (1347, 1348), (1350, 1357), (1359, 1371), (1373, 1382), (1390, 1416), (1418, 1419), (1421, 1433), (1435, 1445), (1447, 1456), (1458, 1481), (1484, 1486), (1488, 1488), (1490, 1521), (1523, 1534), (1537, 1537), (1539, 1539), (1542, 1544), (1551, 1551), (1553, 1554), (1556, 1558), (1560, 1560), (1562, 1564), (1566, 1569), (1571, 1572), (1575, 1576), (1578, 1589), (1591, 1593), (1598, 1600), (1602, 1606), (1609, 1610), (1612, 1615), (1617, 1622), (1624, 1650), (1652, 1668), (1670, 1676), (1678, 1690), (1692, 1698), (1701, 1740), (1742, 1742), (1745, 1748), (1750, 1766), (1769, 1770), (1772, 1779), (1781, 1783), (1785, 1786), (1788, 1833), (1835, 1838), (1840, 1851), (1853, 1857), (1859, 1864), (1866, 1887), (3000, 3001), (3003, 3026), (3028, 3062), (3064, 3074), (3076, 3077), (3079, 3112), (3116, 3171), (3173, 3179), (3181, 3193), (3197, 3203), (3218, 3218), (3231, 3231), (3235, 3238), (3500, 3645), (3647, 3682), (3684, 3748), (3750, 3914), (3916, 3968), (3970, 4017), (4019, 4037), (4039, 4092), (4094, 4102), (4105, 4112), (4114, 4168), (6000, 6108), (6110, 6140)];

/// The numbers MySQL 8.4 sends a client that are about the server's state rather than the
/// statement: a lock waited for, refused or deadlocked, a statement interrupted or a session killed,
/// a limit on memory, connections, threads, prepared statements or time, the network and the
/// connection's sign-in, the session's own locks and transaction, a read-only server, files, disks and
/// a table's storage, a table whose definition changed while it was prepared, and a federated table's
/// remote server. By number, each with its name. `ER_UNKNOWN_ERROR` is not one: Vitess refuses a
/// statement it cannot plan with it ([`mysql`] reads its message). Nor is `ER_ACCESS_DENIED_ERROR`,
/// which a prepare answers for a privilege, the sign-in that also raises it being the connection's.
pub const MYSQL_SERVER: &[(u16, &str)] = &[
    (1004, "ER_CANT_CREATE_FILE"),
    (1013, "ER_CANT_GET_STAT"),
    (1015, "ER_CANT_LOCK"),
    (1016, "ER_CANT_OPEN_FILE"),
    (1017, "ER_FILE_NOT_FOUND"),
    (1020, "ER_CHECKREAD"),
    (1024, "ER_ERROR_ON_READ"),
    (1026, "ER_ERROR_ON_WRITE"),
    (1027, "ER_FILE_USED"),
    (1030, "ER_GET_ERRNO"),
    (1032, "ER_KEY_NOT_FOUND"),
    (1034, "ER_NOT_KEYFILE"),
    (1037, "ER_OUTOFMEMORY"),
    (1038, "ER_OUT_OF_SORTMEMORY"),
    (1040, "ER_CON_COUNT_ERROR"),
    (1041, "ER_OUT_OF_RESOURCES"),
    (1042, "ER_BAD_HOST_ERROR"),
    (1043, "ER_HANDSHAKE_ERROR"),
    (1047, "ER_UNKNOWN_COM_ERROR"),
    (1053, "ER_SERVER_SHUTDOWN"),
    (1079, "ER_SHUTDOWN_COMPLETE"),
    (1099, "ER_TABLE_NOT_LOCKED_FOR_WRITE"),
    (1100, "ER_TABLE_NOT_LOCKED"),
    (1114, "ER_RECORD_FILE_FULL"),
    (1129, "ER_HOST_IS_BLOCKED"),
    (1130, "ER_HOST_NOT_PRIVILEGED"),
    (1135, "ER_CANT_CREATE_THREAD"),
    (1152, "ER_ABORTING_CONNECTION"),
    (1153, "ER_NET_PACKET_TOO_LARGE"),
    (1154, "ER_NET_READ_ERROR_FROM_PIPE"),
    (1155, "ER_NET_FCNTL_ERROR"),
    (1156, "ER_NET_PACKETS_OUT_OF_ORDER"),
    (1157, "ER_NET_UNCOMPRESS_ERROR"),
    (1158, "ER_NET_READ_ERROR"),
    (1159, "ER_NET_READ_INTERRUPTED"),
    (1160, "ER_NET_ERROR_ON_WRITE"),
    (1161, "ER_NET_WRITE_INTERRUPTED"),
    (1184, "ER_NEW_ABORTING_CONNECTION"),
    (1192, "ER_LOCK_OR_ACTIVE_TRANSACTION"),
    (1194, "ER_CRASHED_ON_USAGE"),
    (1195, "ER_CRASHED_ON_REPAIR"),
    (1203, "ER_TOO_MANY_USER_CONNECTIONS"),
    (1205, "ER_LOCK_WAIT_TIMEOUT"),
    (1206, "ER_LOCK_TABLE_FULL"),
    (1207, "ER_READ_ONLY_TRANSACTION"),
    (1213, "ER_LOCK_DEADLOCK"),
    (1226, "ER_USER_LIMIT_REACHED"),
    (1290, "ER_OPTION_PREVENTS_STATEMENT"),
    (1297, "ER_GET_TEMPORARY_ERRMSG"),
    (1317, "ER_QUERY_INTERRUPTED"),
    (1412, "ER_TABLE_DEF_CHANGED"),
    (1429, "ER_CONNECT_TO_FOREIGN_DATA_SOURCE"),
    (1430, "ER_QUERY_ON_FOREIGN_DATA_SOURCE"),
    (1461, "ER_MAX_PREPARED_STMT_COUNT_REACHED"),
    (1614, "ER_XA_RBDEADLOCK"),
    (1615, "ER_NEED_REPREPARE"),
    (1637, "ER_TOO_MANY_CONCURRENT_TRXS"),
    (1689, "ER_LOCK_ABORTED"),
    (1698, "ER_ACCESS_DENIED_NO_PASSWORD_ERROR"),
    (1712, "ER_INDEX_CORRUPT"),
    (1792, "ER_CANT_EXECUTE_IN_READ_ONLY_TRANSACTION"),
    (1812, "ER_TABLESPACE_MISSING"),
    (1814, "ER_TABLESPACE_DISCARDED"),
    (1817, "ER_INNODB_INDEX_CORRUPT"),
    (1820, "ER_MUST_CHANGE_PASSWORD"),
    (1836, "ER_READ_ONLY_MODE"),
    (1862, "ER_MUST_CHANGE_PASSWORD_LOGIN"),
    (1873, "ER_ACCESS_DENIED_CHANGE_USER_ERROR"),
    (1874, "ER_INNODB_READ_ONLY"),
    (1877, "ER_TABLE_CORRUPT"),
    (1878, "ER_TEMP_FILE_WRITE_FAILURE"),
    (3000, "ER_FILE_CORRUPT"),
    (3015, "ER_ENGINE_OUT_OF_MEMORY"),
    (3024, "ER_QUERY_TIMEOUT"),
    (3032, "ER_SERVER_OFFLINE_MODE"),
    (3058, "ER_USER_LOCK_DEADLOCK"),
    (3118, "ER_ACCOUNT_HAS_BEEN_LOCKED"),
    (3132, "ER_LOCKING_SERVICE_DEADLOCK"),
    (3159, "ER_SECURE_TRANSPORT_REQUIRED"),
    (3168, "ER_SERVER_ISNT_AVAILABLE"),
    (3169, "ER_SESSION_WAS_KILLED"),
    (3170, "ER_CAPACITY_EXCEEDED"),
    (3177, "ER_LOCK_REFUSED_BY_ENGINE"),
    (3510, "ER_TABLESPACE_MISSING_WITH_NAME"),
    (3572, "ER_LOCK_NOWAIT"),
    (3656, "ER_RESOURCE_GROUP_BUSY"),
    (3675, "ER_DISK_FULL_NOWAIT"),
    (3782, "ER_GR_HOLD_KILLED"),
    (3885, "ER_DA_UNKNOWN_ERROR_NUMBER"),
    (3950, "ER_DA_OOM"),
    (3955, "ER_USER_ACCESS_DENIED_FOR_USER_ACCOUNT_BLOCKED_BY_PASSWORD_LOCK"),
    (3985, "ER_MDL_OUT_OF_RESOURCES"),
    (4031, "ER_CLIENT_INTERACTION_TIMEOUT"),
    (4081, "ER_DA_GLOBAL_CONN_LIMIT"),
    (4082, "ER_DA_CONN_LIMIT"),
];

/// Every number ClickHouse 26.10 defines, as runs of consecutive numbers, `(first, last)`: the run
/// time's list (src/error-codes-clickhouse.ts), which a test holds this one to.
const CLICKHOUSE_NUMBERS: &[(u32, u32)] = &[(1, 4), (6, 13), (15, 16), (19, 20), (23, 28), (32, 44), (46, 53), (55, 60), (62, 63), (68, 96), (99, 102), (104, 104), (107, 117), (119, 125), (127, 131), (133, 135), (137, 139), (141, 142), (145, 145), (152, 153), (156, 156), (158, 162), (164, 170), (172, 174), (179, 184), (186, 187), (190, 196), (198, 199), (201, 204), (206, 221), (223, 244), (246, 246), (248, 248), (251, 258), (260, 266), (269, 275), (277, 277), (279, 291), (293, 295), (297, 309), (318, 319), (321, 321), (336, 336), (341, 342), (344, 347), (349, 349), (352, 355), (358, 364), (366, 366), (369, 378), (380, 404), (406, 408), (410, 413), (415, 425), (427, 427), (429, 461), (463, 469), (471, 484), (487, 487), (489, 493), (495, 504), (507, 521), (523, 525), (527, 532), (535, 549), (552, 611), (614, 632), (634, 643), (645, 645), (647, 655), (659, 665), (667, 679), (690, 731), (733, 785), (900, 907), (999, 1024)];

/// The numbers ClickHouse 26.10 defines that are about the server's state rather than the statement:
/// a cancel or a timeout, a limit on memory, queries, connections, rows or bytes read, the network,
/// files, disks and remote storage (`s3()`, `url()`, HDFS, Azure), replicas, shards, the distributed
/// cache and Keeper, a session or a sign-in, a table dropped or a part locked meanwhile, and the
/// server's own fault, a corrupted part or a signal. By number, each with its name. `LOGICAL_ERROR`
/// is not one: the analyzer raises it for a statement's shape, and the schema's fingerprint carries
/// the server's version, so an analysis kept with it goes with the server that raised it. Nor is
/// `CANNOT_EXTRACT_TABLE_STRUCTURE`, which `format()` raises for the data a statement gives it, as a
/// value the engine tries in a parameter's place does.
pub const CLICKHOUSE_SERVER: &[(u32, &str)] = &[
    (23, "CANNOT_READ_FROM_ISTREAM"),
    (24, "CANNOT_WRITE_TO_OSTREAM"),
    (33, "CANNOT_READ_ALL_DATA"),
    (39, "TOO_LARGE_SIZE_COMPRESSED"),
    (40, "CHECKSUM_DOESNT_MATCH"),
    (74, "CANNOT_READ_FROM_FILE_DESCRIPTOR"),
    (75, "CANNOT_WRITE_TO_FILE_DESCRIPTOR"),
    (76, "CANNOT_OPEN_FILE"),
    (77, "CANNOT_CLOSE_FILE"),
    (86, "RECEIVED_ERROR_FROM_REMOTE_IO_SERVER"),
    (87, "CANNOT_SEEK_THROUGH_FILE"),
    (88, "CANNOT_TRUNCATE_FILE"),
    (91, "SIZES_OF_MARKS_FILES_ARE_INCONSISTENT"),
    (95, "CANNOT_READ_FROM_SOCKET"),
    (96, "CANNOT_WRITE_TO_SOCKET"),
    (99, "UNKNOWN_PACKET_FROM_CLIENT"),
    (100, "UNKNOWN_PACKET_FROM_SERVER"),
    (101, "UNEXPECTED_PACKET_FROM_CLIENT"),
    (102, "UNEXPECTED_PACKET_FROM_SERVER"),
    (107, "FILE_DOESNT_EXIST"),
    (109, "CANNOT_BLOCK_SIGNAL"),
    (110, "CANNOT_UNBLOCK_SIGNAL"),
    (111, "CANNOT_MANIPULATE_SIGSET"),
    (112, "CANNOT_WAIT_FOR_SIGNAL"),
    (113, "THERE_IS_NO_SESSION"),
    (114, "CANNOT_CLOCK_GETTIME"),
    (156, "DICTIONARIES_WAS_NOT_LOADED"),
    (158, "TOO_MANY_ROWS"),
    (159, "TIMEOUT_EXCEEDED"),
    (160, "TOO_SLOW"),
    (172, "CANNOT_CREATE_DIRECTORY"),
    (173, "CANNOT_ALLOCATE_MEMORY"),
    (191, "SET_SIZE_LIMIT_EXCEEDED"),
    (192, "UNKNOWN_USER"),
    (193, "WRONG_PASSWORD"),
    (194, "REQUIRED_PASSWORD"),
    (198, "DNS_ERROR"),
    (201, "QUOTA_EXCEEDED"),
    (202, "TOO_MANY_SIMULTANEOUS_QUERIES"),
    (203, "NO_FREE_CONNECTION"),
    (209, "SOCKET_TIMEOUT"),
    (210, "NETWORK_ERROR"),
    (218, "TABLE_IS_DROPPED"),
    (225, "NO_ZOOKEEPER"),
    (226, "NO_FILE_IN_DATA_PART"),
    (227, "UNEXPECTED_FILE_IN_DATA_PART"),
    (228, "BAD_SIZE_OF_FILE_IN_DATA_PART"),
    (234, "NO_REPLICA_HAS_PART"),
    (236, "ABORTED"),
    (241, "MEMORY_LIMIT_EXCEEDED"),
    (242, "TABLE_IS_READ_ONLY"),
    (243, "NOT_ENOUGH_SPACE"),
    (244, "UNEXPECTED_ZOOKEEPER_ERROR"),
    (246, "CORRUPTED_DATA"),
    (254, "NO_ACTIVE_REPLICAS"),
    (255, "TOO_MANY_RETRIES_TO_FETCH_PARTS"),
    (265, "NO_AVAILABLE_REPLICA"),
    (270, "CANNOT_COMPRESS"),
    (271, "CANNOT_DECOMPRESS"),
    (272, "CANNOT_IO_SUBMIT"),
    (273, "CANNOT_IO_GETEVENTS"),
    (274, "AIO_READ_ERROR"),
    (275, "AIO_WRITE_ERROR"),
    (279, "ALL_CONNECTION_TRIES_FAILED"),
    (285, "TOO_FEW_LIVE_REPLICAS"),
    (289, "REPLICA_IS_NOT_IN_QUORUM"),
    (294, "CANNOT_WRITE_TO_FILE"),
    (297, "SHARD_HAS_NO_CONNECTIONS"),
    (298, "CANNOT_PIPE"),
    (304, "CANNOT_WAITPID"),
    (307, "TOO_MANY_BYTES"),
    (308, "UNEXPECTED_NODE_IN_ZOOKEEPER"),
    (341, "UNFINISHED"),
    (363, "CANNOT_CREATE_IO_BUFFER"),
    (364, "RECEIVED_ERROR_TOO_MANY_REQUESTS"),
    (369, "ALL_REPLICAS_ARE_STALE"),
    (372, "SESSION_NOT_FOUND"),
    (373, "SESSION_IS_LOCKED"),
    (384, "PART_IS_TEMPORARILY_LOCKED"),
    (394, "QUERY_WAS_CANCELLED"),
    (396, "TOO_MANY_ROWS_OR_BYTES"),
    (410, "EXTERNAL_SERVER_IS_NOT_RESPONDING"),
    (411, "PTHREAD_ERROR"),
    (413, "CANNOT_SET_SIGNAL_HANDLER"),
    (415, "ALL_REPLICAS_LOST"),
    (416, "REPLICA_STATUS_CHANGED"),
    (425, "SYSTEM_ERROR"),
    (439, "CANNOT_SCHEDULE_TASK"),
    (454, "OPENSSL_ERROR"),
    (459, "CANNOT_SET_THREAD_PRIORITY"),
    (468, "CANNOT_PTHREAD_ATTR"),
    (473, "DEADLOCK_AVOIDED"),
    (483, "TOO_MANY_REDIRECTS"),
    (499, "S3_ERROR"),
    (500, "AZURE_BLOB_STORAGE_ERROR"),
    (502, "CANNOT_SIGQUEUE"),
    (510, "CACHE_DICTIONARY_UPDATE_FAIL"),
    (516, "AUTHENTICATION_FAILED"),
    (519, "NO_REMOTE_SHARD_AVAILABLE"),
    (532, "LDAP_ERROR"),
    (571, "DATABASE_REPLICATION_FAILED"),
    (574, "DISTRIBUTED_TOO_MANY_PENDING_BYTES"),
    (576, "KERBEROS_ERROR"),
    (586, "CANNOT_CREATE_FILE"),
    (614, "POSTGRESQL_CONNECTION_FAILURE"),
    (625, "IO_SETUP_ERROR"),
    (629, "HTTP_RANGE_NOT_SATISFIABLE"),
    (631, "UNKNOWN_FILE_SIZE"),
    (638, "SNAPPY_UNCOMPRESS_FAILED"),
    (639, "SNAPPY_COMPRESS_FAILED"),
    (641, "CANNOT_APPEND_TO_FILE"),
    (660, "HDFS_ERROR"),
    (661, "CANNOT_SEND_SIGNAL"),
    (677, "THREAD_WAS_CANCELED"),
    (678, "IO_URING_INIT_FAILED"),
    (679, "IO_URING_SUBMIT_ERROR"),
    (696, "ASYNC_LOAD_CANCELED"),
    (700, "USER_SESSION_LIMIT_EXCEEDED"),
    (709, "CANNOT_GET_REPLICATED_DATABASE_SNAPSHOT"),
    (713, "BROKEN_PROJECTION"),
    (720, "USER_EXPIRED"),
    (722, "ASYNC_LOAD_WAIT_FAILED"),
    (733, "TABLE_IS_BEING_RESTARTED"),
    (734, "CANNOT_WRITE_AFTER_BUFFER_CANCELED"),
    (735, "QUERY_WAS_CANCELLED_BY_CLIENT"),
    (738, "PART_IS_LOCKED"),
    (740, "POTENTIALLY_BROKEN_DATA_PART"),
    (744, "SESSION_ID_EMPTY"),
    (745, "SERVER_OVERLOADED"),
    (747, "FILECACHE_CANNOT_WRITE_THROUGH_CACHE_WITH_CONCURRENT_READS"),
    (749, "TCP_CONNECTION_LIMIT_REACHED"),
    (751, "ARROWFLIGHT_CONNECTION_FAILURE"),
    (752, "ARROWFLIGHT_FETCH_SCHEMA_ERROR"),
    (757, "CANNOT_GET_THREAD_PRIORITY"),
    (762, "HTTP_CONNECTION_LIMIT_REACHED"),
    (763, "SESSION_REFUSED"),
    (771, "CACHE_CANNOT_WRITE_TO_CACHE_DISK"),
    (776, "RESOURCE_LIMIT_EXCEEDED"),
    (777, "MEMORY_RESERVATION_KILLED"),
    (778, "MEMORY_RESERVATION_FAILED"),
    (780, "SW_SERVER_NO_WORKERS_AVAILABLE"),
    (900, "DISTRIBUTED_CACHE_ERROR"),
    (901, "CANNOT_USE_DISTRIBUTED_CACHE"),
    (903, "DISTRIBUTED_CACHE_REGISTRY_SHUTDOWN"),
    (904, "TOO_MANY_UNAVAILABLE_SHARDS"),
    (905, "DISTRIBUTED_CACHE_SERVER_IN_BACKOFF"),
    (907, "DISTRIBUTED_CACHE_UNEXPECTED_SERVER_CERTIFICATE"),
    (999, "KEEPER_EXCEPTION"),
    (1000, "POCO_EXCEPTION"),
    (1001, "STD_EXCEPTION"),
    (1002, "UNKNOWN_EXCEPTION"),
    (1005, "STALE_VERSION"),
    (1008, "TEMPORARY_DATA_NOT_IN_CACHE"),
    (1009, "S3_OBJECT_CHANGED_DURING_READ"),
    (1015, "FILE_CHANGED_DURING_READ"),
    (1016, "TABLE_SIZE_LIMIT_EXCEEDED"),
    (1018, "EXCHANGE_PEER_DISCONNECTED"),
    (1019, "QUERY_SLOT_ACQUISITION_TIMEOUT"),
    (1020, "MEMORY_RESERVATION_ACQUISITION_TIMEOUT"),
    (1021, "AZURE_OBJECT_CHANGED_DURING_READ"),
];

/// The numbers MySQL 8.4 sends a client for a privilege the role lacks, which [`kept`] keeps no
/// analysis with. By number, each with its name.
pub const MYSQL_PRIVILEGE: &[(u16, &str)] = &[
    (1044, "ER_DBACCESS_DENIED_ERROR"),
    (1045, "ER_ACCESS_DENIED_ERROR"),
    (1142, "ER_TABLEACCESS_DENIED_ERROR"),
    (1143, "ER_COLUMNACCESS_DENIED_ERROR"),
    (1227, "ER_SPECIFIC_ACCESS_DENIED_ERROR"),
    (1370, "ER_PROCACCESS_DENIED_ERROR"),
    (3879, "ER_DB_ACCESS_DENIED"),
    (6038, "ER_SPECIFIC_ACCESS_DENIED"),
];

/// The numbers ClickHouse 26.10 defines for a privilege the user lacks, which [`kept`] keeps no
/// analysis with. By number, each with its name.
pub const CLICKHOUSE_PRIVILEGE: &[(u32, &str)] = &[
    (291, "DATABASE_ACCESS_DENIED"),
    (481, "PATH_ACCESS_DENIED"),
    (482, "DICTIONARY_ACCESS_DENIED"),
    (497, "ACCESS_DENIED"),
    (673, "RESOURCE_ACCESS_DENIED"),
    (711, "FILECACHE_ACCESS_DENIED"),
    (1022, "SEMI_ANTI_JOIN_COLUMN_ACCESS_DENIED"),
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_postgres_error_is_the_statements_by_its_class() {
        for code in ["42703", "42P01", "42601", "42501", "22P02", "0A000", "3F000", "54001", "55000", "26000"] {
            assert!(postgres(code), "{code}");
        }
        // A deadlock, memory, a lock, a cancel, the system, an internal error, the connection, a
        // failed transaction, a statement name a pooler's shared session already holds.
        for code in ["40P01", "40001", "53200", "55P03", "55006", "57014", "57P01", "58P01", "XX000", "08006", "25P02", "42P05", "", "42", "4\u{e9}12"] {
            assert!(!postgres(code), "{code}");
        }
    }

    #[test]
    fn a_mysql_error_is_the_statements_by_its_number() {
        // A parse error, a column not found, a statement prepare does not take, a mix of collations,
        // a privilege, and the unknown error, with which Vitess refuses a statement it cannot plan.
        for number in [1064, 1054, 1295, 1267, 1146, 1044, 1045, 3143, 1105] {
            assert!(mysql(number, None, Some("VT12001: unsupported: subquery in GROUP BY")), "{number}");
        }
        // A deadlock, a lock timeout, an interrupt, the prepared statements' limit (42000), a table
        // whose definition changed, out of memory, a tablespace missing, a connection's memory limit.
        for number in [1213, 1205, 1317, 1461, 1615, 1412, 1037, 3024, 3169, 1812, 4082] {
            assert!(!mysql(number, Some("HY000"), None), "{number}");
        }
        // Vitess's own failures, which it answers with the unknown error too.
        for message in ["target: shop.-.primary: no healthy tablet available for 'keyspace:\"shop\" shard:\"-\" tablet_type:PRIMARY'", "operation not allowed in state NOT_SERVING", "primary is not serving, there may be a reparent operation in progress"] {
            assert!(!mysql(1105, Some("HY000"), Some(message)), "{message}");
        }
        // A newer server's number, by its SQLSTATE's class.
        assert!(mysql(6999, Some("42000"), None));
        assert!(!mysql(6999, Some("HY000"), None));
        assert!(!mysql(6999, None, None));
        assert!(!of_statement(Dialect::Mysql, "6999"));
        assert!(of_statement(Dialect::Mysql, "1064"));
        assert!(!of_statement(Dialect::Mysql, "ER_PARSE_ERROR"));
    }

    #[test]
    fn a_clickhouse_error_is_the_statements_by_its_number() {
        // An unknown identifier, a syntax error, an argument of the wrong type, text that does not
        // parse, a denied access, a logical error, which the analyzer raises for a statement's shape,
        // and a statement too long to parse in the parser's bound.
        for number in [47, 62, 43, 6, 32, 497, 49, 718] {
            assert!(clickhouse(number), "{number}");
        }
        // A cancel, a timeout, too many queries, memory, the network, Keeper, a table being
        // restarted, a number no version defines.
        for number in [394, 159, 202, 241, 209, 210, 999, 733, 1_000_000] {
            assert!(!clickhouse(number), "{number}");
        }
        assert!(!of_statement(Dialect::Clickhouse, "Code: 47"));
    }

    #[test]
    fn a_sqlite_error_is_the_statements_by_its_code() {
        for code in ["SQLITE_ERROR", "SQLITE_ERROR_MISSING_COLLSEQ", "SQLITE_AUTH", "SQLITE_TOOBIG"] {
            assert!(sqlite(code), "{code}");
        }
        for code in ["SQLITE_BUSY", "SQLITE_LOCKED_SHAREDCACHE", "SQLITE_NOMEM", "SQLITE_IOERR_READ", "SQLITE_SCHEMA", "SQLITE_AUTH_USER", "SQLITE_ERROR_RETRY", "SQLITE_ERRORS", ""] {
            assert!(!sqlite(code), "{code}");
        }
    }

    #[test]
    fn an_analysis_refused_for_a_privilege_is_not_kept() {
        assert!(kept(Dialect::Postgres, "42703") && !kept(Dialect::Postgres, "42501") && !kept(Dialect::Postgres, "40P01"));
        assert!(kept(Dialect::Mysql, "1054") && !kept(Dialect::Mysql, "1142") && !kept(Dialect::Mysql, "1045") && !kept(Dialect::Mysql, "1213"));
        assert!(kept(Dialect::Clickhouse, "47") && !kept(Dialect::Clickhouse, "497") && !kept(Dialect::Clickhouse, "394"));
    }

    #[test]
    fn the_lists_are_sorted_and_within_their_tables() {
        assert!(MYSQL_NUMBERS.windows(2).all(|w| w[0].1 < w[1].0) && MYSQL_NUMBERS.iter().all(|&(a, b)| a <= b));
        assert!(CLICKHOUSE_NUMBERS.windows(2).all(|w| w[0].1 < w[1].0) && CLICKHOUSE_NUMBERS.iter().all(|&(a, b)| a <= b));
        for list in [MYSQL_SERVER, MYSQL_PRIVILEGE] {
            assert!(list.windows(2).all(|w| w[0].0 < w[1].0) && list.iter().all(|&(n, _)| known(MYSQL_NUMBERS, n)));
        }
        for list in [CLICKHOUSE_SERVER, CLICKHOUSE_PRIVILEGE] {
            assert!(list.windows(2).all(|w| w[0].0 < w[1].0) && list.iter().all(|&(n, _)| known(CLICKHOUSE_NUMBERS, n)));
        }
    }
}
