// Whether a database's error is the statement's own answer: one the server raises because of what
// the statement says, read against the schema and the session's settings, so that the same
// statement over the same schema raises it again. Every other error (another session's lock or a
// deadlock, a cancel, a limit on time, memory or connections, the network, the server's own fault)
// says something of the moment, so a host fails the analysis with it and nothing is kept in
// `reylo/.cache/`. The engine's hosts decide this in engine/native/src/server_errors.rs, which these
// lists copy and a test holds them to; the errors are listed by what they are allowed to be, so a
// code neither knows, as one a newer server adds, is not the statement's.
import type { Dialect } from "./engine-core.ts";
import { CLICKHOUSE_CODES } from "./error-codes-clickhouse.ts";
import { MYSQL_CODES } from "./error-codes-mysql.ts";

/**
 * Whether an error with the database's own `code` (Postgres's SQLSTATE, MySQL's or ClickHouse's
 * error number, SQLite's result code's name) is the statement's. `sqlState` is the SQLSTATE MySQL
 * sends beside its number, which decides a number MySQL 8.4 does not send, and `message` the error's
 * text, which tells Vitess's failures from its refusals of a statement under one number.
 */
export function statementError(dialect: Dialect, code: string | undefined, sqlState?: string, message?: string): boolean {
  if (code === undefined) return false;
  switch (dialect) {
    case "postgres":
      return code === "55000" || code === "26000" || (/^(0A|22|3F|42|54)[0-9A-Z]{3}$/.test(code) && code !== "42P05");
    case "mysql": {
      const name = /^\d+$/.test(code) ? mysqlNames().get(Number(code)) : undefined;
      if (name !== undefined) return !MYSQL_SERVER.has(name) && !(name === "ER_UNKNOWN_ERROR" && message !== undefined && vitessState(message));
      return /^\d+$/.test(code) && sqlState !== undefined && /^(42|22|21|0A|3D)...$/.test(sqlState);
    }
    case "clickhouse": {
      const name = /^\d+$/.test(code) ? clickhouseNames().get(Number(code)) : undefined;
      return name !== undefined && !CLICKHOUSE_SERVER.has(name);
    }
    case "sqlite":
      return ["SQLITE_ERROR", "SQLITE_ERROR_MISSING_COLLSEQ", "SQLITE_AUTH", "SQLITE_TOOBIG"].includes(code);
  }
}

/** Whether an analysis whose diagnostic carries `code` may be kept in `reylo/.cache/`: the statement's error, and none a privilege or a remote source's data decides, since neither a grant nor an upload moves a schema's fingerprint. */
export function keptError(dialect: Dialect, code: string): boolean {
  if (!statementError(dialect, code)) return false;
  if (dialect === "postgres") return code !== "42501";
  if (dialect === "mysql") return !MYSQL_PRIVILEGE.has(mysqlNames().get(Number(code))!);
  if (dialect === "clickhouse") return ![CLICKHOUSE_PRIVILEGE, CLICKHOUSE_INFERRED].some((list) => list.has(clickhouseNames().get(Number(code))!));
  return true;
}

/** Whether a Vitess message is about a tablet's or the cluster's state rather than the statement (engine/native/src/server_errors.rs, `vitess_state`). */
function vitessState(message: string): boolean {
  return ["no healthy tablet available", "found no available tablet", "operation not allowed in state", "there may be a reparent operation in progress", "current keyspace is being resharded", "tx engine can't accept new connections", "code = Unavailable"].some((words) => message.includes(words));
}

let mysqlByNumber: Map<number, string> | undefined;
function mysqlNames(): Map<number, string> {
  return (mysqlByNumber ??= new Map(MYSQL_CODES.flatMap(([first, names]) => names.map((name, i) => [first + i, name] as const))));
}

let clickhouseByNumber: Map<number, string> | undefined;
function clickhouseNames(): Map<number, string> {
  return (clickhouseByNumber ??= new Map(CLICKHOUSE_CODES.flatMap(([first, names]) => names.map((name, i) => [first + i, name] as const))));
}

/** The errors MySQL 8.4 sends a client that are about the server's state rather than the statement (engine/native/src/server_errors.rs, `MYSQL_SERVER`). */
export const MYSQL_SERVER = new Set<string>(["ER_INTERNAL_ERROR", "ER_CANT_CREATE_FILE", "ER_CANT_GET_STAT", "ER_CANT_LOCK", "ER_CANT_OPEN_FILE", "ER_FILE_NOT_FOUND", "ER_CHECKREAD", "ER_ERROR_ON_READ", "ER_ERROR_ON_WRITE", "ER_FILE_USED", "ER_GET_ERRNO", "ER_KEY_NOT_FOUND", "ER_OUTOFMEMORY", "ER_OUT_OF_SORTMEMORY", "ER_CON_COUNT_ERROR", "ER_OUT_OF_RESOURCES", "ER_BAD_HOST_ERROR", "ER_HANDSHAKE_ERROR", "ER_UNKNOWN_COM_ERROR", "ER_SERVER_SHUTDOWN", "ER_SHUTDOWN_COMPLETE", "ER_TABLE_NOT_LOCKED_FOR_WRITE", "ER_TABLE_NOT_LOCKED", "ER_RECORD_FILE_FULL", "ER_HOST_IS_BLOCKED", "ER_HOST_NOT_PRIVILEGED", "ER_CANT_CREATE_THREAD", "ER_ABORTING_CONNECTION", "ER_NET_PACKET_TOO_LARGE", "ER_NET_READ_ERROR_FROM_PIPE", "ER_NET_FCNTL_ERROR", "ER_NET_PACKETS_OUT_OF_ORDER", "ER_NET_UNCOMPRESS_ERROR", "ER_NET_READ_ERROR", "ER_NET_READ_INTERRUPTED", "ER_NET_ERROR_ON_WRITE", "ER_NET_WRITE_INTERRUPTED", "ER_NEW_ABORTING_CONNECTION", "ER_LOCK_OR_ACTIVE_TRANSACTION", "ER_CRASHED_ON_USAGE", "ER_CRASHED_ON_REPAIR", "ER_TOO_MANY_USER_CONNECTIONS", "ER_LOCK_WAIT_TIMEOUT", "ER_LOCK_TABLE_FULL", "ER_READ_ONLY_TRANSACTION", "ER_LOCK_DEADLOCK", "ER_USER_LIMIT_REACHED", "ER_OPTION_PREVENTS_STATEMENT", "ER_QUERY_INTERRUPTED", "ER_TABLE_DEF_CHANGED", "ER_CONNECT_TO_FOREIGN_DATA_SOURCE", "ER_QUERY_ON_FOREIGN_DATA_SOURCE", "ER_MAX_PREPARED_STMT_COUNT_REACHED", "ER_XA_RBDEADLOCK", "ER_NEED_REPREPARE", "ER_TOO_MANY_CONCURRENT_TRXS", "ER_LOCK_ABORTED", "ER_ACCESS_DENIED_NO_PASSWORD_ERROR", "ER_CANT_EXECUTE_IN_READ_ONLY_TRANSACTION", "ER_MUST_CHANGE_PASSWORD", "ER_MUST_CHANGE_PASSWORD_LOGIN", "ER_READ_ONLY_MODE", "ER_ACCESS_DENIED_CHANGE_USER_ERROR", "ER_INNODB_READ_ONLY", "ER_TABLE_CORRUPT", "ER_TEMP_FILE_WRITE_FAILURE", "ER_ENGINE_OUT_OF_MEMORY", "ER_QUERY_TIMEOUT", "ER_SERVER_OFFLINE_MODE", "ER_USER_LOCK_DEADLOCK", "ER_ACCOUNT_HAS_BEEN_LOCKED", "ER_LOCKING_SERVICE_DEADLOCK", "ER_SECURE_TRANSPORT_REQUIRED", "ER_SERVER_ISNT_AVAILABLE", "ER_SESSION_WAS_KILLED", "ER_CAPACITY_EXCEEDED", "ER_LOCK_REFUSED_BY_ENGINE", "ER_LOCK_NOWAIT", "ER_RESOURCE_GROUP_BUSY", "ER_DISK_FULL_NOWAIT", "ER_GR_HOLD_KILLED", "ER_DA_UNKNOWN_ERROR_NUMBER", "ER_DA_OOM", "ER_USER_ACCESS_DENIED_FOR_USER_ACCOUNT_BLOCKED_BY_PASSWORD_LOCK", "ER_MDL_OUT_OF_RESOURCES", "ER_CLIENT_INTERACTION_TIMEOUT", "ER_NOT_KEYFILE", "ER_GET_TEMPORARY_ERRMSG", "ER_INDEX_CORRUPT", "ER_TABLESPACE_MISSING", "ER_TABLESPACE_DISCARDED", "ER_INNODB_INDEX_CORRUPT", "ER_FILE_CORRUPT", "ER_TABLESPACE_MISSING_WITH_NAME", "ER_DA_GLOBAL_CONN_LIMIT", "ER_DA_CONN_LIMIT"]);

/** The errors ClickHouse 26.10 defines that are about the server's state rather than the statement (engine/native/src/server_errors.rs, `CLICKHOUSE_SERVER`). */
export const CLICKHOUSE_SERVER = new Set<string>(["CANNOT_READ_FROM_ISTREAM", "CANNOT_WRITE_TO_OSTREAM", "CANNOT_READ_ALL_DATA", "TOO_LARGE_SIZE_COMPRESSED", "CHECKSUM_DOESNT_MATCH", "CANNOT_READ_FROM_FILE_DESCRIPTOR", "CANNOT_WRITE_TO_FILE_DESCRIPTOR", "CANNOT_OPEN_FILE", "CANNOT_CLOSE_FILE", "RECEIVED_ERROR_FROM_REMOTE_IO_SERVER", "CANNOT_SEEK_THROUGH_FILE", "CANNOT_TRUNCATE_FILE", "SIZES_OF_MARKS_FILES_ARE_INCONSISTENT", "CANNOT_READ_FROM_SOCKET", "CANNOT_WRITE_TO_SOCKET", "UNKNOWN_PACKET_FROM_CLIENT", "UNKNOWN_PACKET_FROM_SERVER", "UNEXPECTED_PACKET_FROM_CLIENT", "UNEXPECTED_PACKET_FROM_SERVER", "FILE_DOESNT_EXIST", "CANNOT_BLOCK_SIGNAL", "CANNOT_UNBLOCK_SIGNAL", "CANNOT_MANIPULATE_SIGSET", "CANNOT_WAIT_FOR_SIGNAL", "THERE_IS_NO_SESSION", "CANNOT_CLOCK_GETTIME", "DICTIONARIES_WAS_NOT_LOADED", "TOO_MANY_ROWS", "TIMEOUT_EXCEEDED", "TOO_SLOW", "CANNOT_CREATE_DIRECTORY", "CANNOT_ALLOCATE_MEMORY", "SET_SIZE_LIMIT_EXCEEDED", "UNKNOWN_USER", "WRONG_PASSWORD", "REQUIRED_PASSWORD", "DNS_ERROR", "QUOTA_EXCEEDED", "TOO_MANY_SIMULTANEOUS_QUERIES", "NO_FREE_CONNECTION", "SOCKET_TIMEOUT", "NETWORK_ERROR", "TABLE_IS_DROPPED", "NO_ZOOKEEPER", "NO_FILE_IN_DATA_PART", "UNEXPECTED_FILE_IN_DATA_PART", "BAD_SIZE_OF_FILE_IN_DATA_PART", "NO_REPLICA_HAS_PART", "ABORTED", "MEMORY_LIMIT_EXCEEDED", "TABLE_IS_READ_ONLY", "NOT_ENOUGH_SPACE", "UNEXPECTED_ZOOKEEPER_ERROR", "CORRUPTED_DATA", "NO_ACTIVE_REPLICAS", "TOO_MANY_RETRIES_TO_FETCH_PARTS", "NO_AVAILABLE_REPLICA", "CANNOT_COMPRESS", "CANNOT_DECOMPRESS", "CANNOT_IO_SUBMIT", "CANNOT_IO_GETEVENTS", "AIO_READ_ERROR", "AIO_WRITE_ERROR", "ALL_CONNECTION_TRIES_FAILED", "TOO_FEW_LIVE_REPLICAS", "REPLICA_IS_NOT_IN_QUORUM", "CANNOT_WRITE_TO_FILE", "SHARD_HAS_NO_CONNECTIONS", "CANNOT_PIPE", "CANNOT_WAITPID", "TOO_MANY_BYTES", "UNEXPECTED_NODE_IN_ZOOKEEPER", "UNFINISHED", "CANNOT_CREATE_IO_BUFFER", "RECEIVED_ERROR_TOO_MANY_REQUESTS", "ALL_REPLICAS_ARE_STALE", "SESSION_NOT_FOUND", "SESSION_IS_LOCKED", "PART_IS_TEMPORARILY_LOCKED", "QUERY_WAS_CANCELLED", "TOO_MANY_ROWS_OR_BYTES", "EXTERNAL_SERVER_IS_NOT_RESPONDING", "PTHREAD_ERROR", "CANNOT_SET_SIGNAL_HANDLER", "ALL_REPLICAS_LOST", "REPLICA_STATUS_CHANGED", "SYSTEM_ERROR", "CANNOT_SCHEDULE_TASK", "OPENSSL_ERROR", "CANNOT_SET_THREAD_PRIORITY", "CANNOT_PTHREAD_ATTR", "DEADLOCK_AVOIDED", "TOO_MANY_REDIRECTS", "S3_ERROR", "AZURE_BLOB_STORAGE_ERROR", "CANNOT_SIGQUEUE", "CACHE_DICTIONARY_UPDATE_FAIL", "AUTHENTICATION_FAILED", "NO_REMOTE_SHARD_AVAILABLE", "LDAP_ERROR", "DATABASE_REPLICATION_FAILED", "DISTRIBUTED_TOO_MANY_PENDING_BYTES", "KERBEROS_ERROR", "CANNOT_CREATE_FILE", "POSTGRESQL_CONNECTION_FAILURE", "IO_SETUP_ERROR", "HTTP_RANGE_NOT_SATISFIABLE", "UNKNOWN_FILE_SIZE", "SNAPPY_UNCOMPRESS_FAILED", "SNAPPY_COMPRESS_FAILED", "CANNOT_APPEND_TO_FILE", "HDFS_ERROR", "CANNOT_SEND_SIGNAL", "THREAD_WAS_CANCELED", "IO_URING_INIT_FAILED", "IO_URING_SUBMIT_ERROR", "ASYNC_LOAD_CANCELED", "USER_SESSION_LIMIT_EXCEEDED", "CANNOT_GET_REPLICATED_DATABASE_SNAPSHOT", "BROKEN_PROJECTION", "USER_EXPIRED", "ASYNC_LOAD_WAIT_FAILED", "CANNOT_WRITE_AFTER_BUFFER_CANCELED", "QUERY_WAS_CANCELLED_BY_CLIENT", "PART_IS_LOCKED", "POTENTIALLY_BROKEN_DATA_PART", "SESSION_ID_EMPTY", "SERVER_OVERLOADED", "FILECACHE_CANNOT_WRITE_THROUGH_CACHE_WITH_CONCURRENT_READS", "TCP_CONNECTION_LIMIT_REACHED", "ARROWFLIGHT_CONNECTION_FAILURE", "ARROWFLIGHT_FETCH_SCHEMA_ERROR", "CANNOT_GET_THREAD_PRIORITY", "HTTP_CONNECTION_LIMIT_REACHED", "SESSION_REFUSED", "CACHE_CANNOT_WRITE_TO_CACHE_DISK", "RESOURCE_LIMIT_EXCEEDED", "MEMORY_RESERVATION_KILLED", "MEMORY_RESERVATION_FAILED", "SW_SERVER_NO_WORKERS_AVAILABLE", "DISTRIBUTED_CACHE_ERROR", "CANNOT_USE_DISTRIBUTED_CACHE", "DISTRIBUTED_CACHE_REGISTRY_SHUTDOWN", "TOO_MANY_UNAVAILABLE_SHARDS", "DISTRIBUTED_CACHE_SERVER_IN_BACKOFF", "DISTRIBUTED_CACHE_UNEXPECTED_SERVER_CERTIFICATE", "KEEPER_EXCEPTION", "POCO_EXCEPTION", "STD_EXCEPTION", "UNKNOWN_EXCEPTION", "STALE_VERSION", "TEMPORARY_DATA_NOT_IN_CACHE", "S3_OBJECT_CHANGED_DURING_READ", "FILE_CHANGED_DURING_READ", "TABLE_SIZE_LIMIT_EXCEEDED", "QUERY_SLOT_ACQUISITION_TIMEOUT", "MEMORY_RESERVATION_ACQUISITION_TIMEOUT", "AZURE_OBJECT_CHANGED_DURING_READ", "TABLE_IS_BEING_RESTARTED", "EXCHANGE_PEER_DISCONNECTED"]);

/** MySQL's errors for a privilege the role lacks (engine/native/src/server_errors.rs, `MYSQL_PRIVILEGE`). */
export const MYSQL_PRIVILEGE = new Set<string>(["ER_DBACCESS_DENIED_ERROR", "ER_ACCESS_DENIED_ERROR", "ER_TABLEACCESS_DENIED_ERROR", "ER_COLUMNACCESS_DENIED_ERROR", "ER_SPECIFIC_ACCESS_DENIED_ERROR", "ER_PROCACCESS_DENIED_ERROR", "ER_DB_ACCESS_DENIED", "ER_SPECIFIC_ACCESS_DENIED"]);

/** ClickHouse's errors for a privilege the user lacks (engine/native/src/server_errors.rs, `CLICKHOUSE_PRIVILEGE`). */
export const CLICKHOUSE_PRIVILEGE = new Set<string>(["DATABASE_ACCESS_DENIED", "PATH_ACCESS_DENIED", "DICTIONARY_ACCESS_DENIED", "ACCESS_DENIED", "RESOURCE_ACCESS_DENIED", "FILECACHE_ACCESS_DENIED", "SEMI_ANTI_JOIN_COLUMN_ACCESS_DENIED"]);

/** ClickHouse's errors for a schema it could not infer from a source's data (engine/native/src/server_errors.rs, `CLICKHOUSE_INFERRED`). */
export const CLICKHOUSE_INFERRED = new Set<string>(["CANNOT_EXTRACT_TABLE_STRUCTURE", "ONLY_NULLS_WHILE_READING_SCHEMA", "CANNOT_DETECT_FORMAT"]);
