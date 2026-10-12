use crate::config::{Connection, SshConfig, TlsMode};
use crate::exporter::{ExportFormat, PageWriter, expand_path, with_default_ext};
use crate::schema::SchemaCache;
use mysql::prelude::Queryable;
use mysql::{Pool, PooledConn, Value};
use std::fs::File;
use std::path::PathBuf;
use std::sync::mpsc::{Receiver, Sender};
use std::time::{Duration, Instant};

/// Cap on rows fetched into the UI to keep rendering snappy.
pub const MAX_ROWS: usize = 10_000;

#[derive(Debug, Clone)]
pub struct ConnInfo {
    pub server_version: String,
    pub database: String,
    pub user: String,
    pub host: String,
    pub port: u16,
    /// Server-assigned id of the session connection; used for KILL QUERY.
    pub conn_id: u64,
    /// Local port of the SSH tunnel when connected through a jump host;
    /// the cancel path must reuse it to reach the same session.
    pub tunnel_port: Option<u16>,
    /// Negotiated TLS cipher as reported by the server; None when the session is plaintext.
    pub ssl_cipher: Option<String>,
}

impl ConnInfo {
    pub fn label(&self) -> String {
        let db = if self.database.is_empty() {
            "-"
        } else {
            &self.database
        };
        format!("{}@{}:{}/{}", self.user, self.host, self.port, db)
    }
}

#[derive(Debug, Clone)]
pub struct QueryResult {
    pub columns: Vec<String>,
    /// Original cell values so exports can preserve types (numbers, NULL).
    pub rows: Vec<Vec<Value>>,
    /// True when rows were dropped because MAX_ROWS was exceeded.
    pub truncated: bool,
    pub elapsed: Duration,
    /// The SELECT that produced this set, when it can be paged with LIMIT/OFFSET.
    pub source: Option<String>,
    /// True when the server may hold rows beyond the last one fetched.
    pub has_more: bool,
}

impl QueryResult {
    /// SQL and page size for the batch following `self.rows`; None when the
    /// statement is not pageable or its own LIMIT window is exhausted.
    pub fn next_page(&self) -> Option<(String, usize)> {
        next_page_sql(self.source.as_deref()?, self.rows.len())
    }
}

#[derive(Debug)]
pub enum DbCommand {
    Connect(Connection),
    Disconnect,
    Query(String),
    /// Fetch one more page of an earlier SELECT; `page` is the LIMIT baked into `sql`.
    QueryPage {
        sql: String,
        page: usize,
    },
    RefreshSchema,
    FetchTableDetail(String, String), // database, table
    ExportAll {
        sql: String,
        path: String,
        format: ExportFormat,
    },
}

#[derive(Debug)]
pub enum DbEvent {
    Connected(ConnInfo),
    ConnectFailed(String),
    QueryDone {
        /// Every result set that produced columns, in statement order.
        results: Vec<QueryResult>,
        /// Status message, e.g. "3 rows affected".
        message: String,
    },
    QueryFailed(String),
    /// Next batch of rows for the result set that requested paging.
    PageDone {
        rows: Vec<Vec<Value>>,
        has_more: bool,
        elapsed: Duration,
    },
    /// Loading the next batch failed; the existing rows stay valid.
    PageFailed(String),
    /// A user-requested cancel could not be delivered; the query keeps running.
    CancelFailed(String),
    SchemaUpdated(SchemaCache),
    TableDetailFetched {
        database: String,
        table: String,
        columns: Vec<String>,
        ddl: String,
    },
    Disconnected,
    ExportProgress(usize),
    ExportDone {
        result: Result<(PathBuf, usize), String>,
    },
}

pub struct DbWorker {
    tx_cmd: Sender<DbCommand>,
    rx_evt: Receiver<DbEvent>,
    /// Clone of the worker's event sender so cancel threads can report back.
    tx_evt: Sender<DbEvent>,
}

impl DbWorker {
    /// Spawn the background DB thread. It owns the pool so that slow queries
    /// never block the UI event loop.
    pub fn spawn() -> Self {
        let (tx_cmd, rx_cmd) = std::sync::mpsc::channel();
        let (tx_evt, rx_evt) = std::sync::mpsc::channel();
        // clone kept on the struct so cancel threads can send events
        let tx_evt2 = tx_evt.clone();
        std::thread::Builder::new()
            .name("db-worker".into())
            .spawn(move || worker_loop(rx_cmd, tx_evt))
            .expect("failed to spawn db worker thread");
        Self {
            tx_cmd,
            rx_evt,
            tx_evt: tx_evt2,
        }
    }

    pub fn send(&self, cmd: DbCommand) {
        let _ = self.tx_cmd.send(cmd);
    }

    /// Non-blocking poll for the next event from the worker.
    pub fn poll_event(&self) -> Option<DbEvent> {
        self.rx_evt.try_recv().ok()
    }

    /// Ask the server to abort the query running on `conn_id` without
    /// killing the session. The worker thread itself is blocked inside the
    /// query, so the KILL runs on a throwaway connection in its own thread.
    pub fn cancel(&self, conn: &Connection, conn_id: u64, tunnel_port: Option<u16>) {
        let conn = conn.clone();
        let tx_evt = self.tx_evt.clone();
        std::thread::Builder::new()
            .name("db-cancel".into())
            .spawn(move || match kill_query(&conn, conn_id, tunnel_port) {
                Ok(()) => {}
                Err(e) => {
                    let _ = tx_evt.send(DbEvent::CancelFailed(e));
                }
            })
            .ok();
    }
}

fn kill_query(conn: &Connection, conn_id: u64, tunnel_port: Option<u16>) -> Result<(), String> {
    let (host, port) = match tunnel_port {
        Some(p) => ("127.0.0.1".to_string(), p),
        None => (conn.host.clone(), conn.port),
    };
    let opts = mysql::OptsBuilder::new()
        .ip_or_hostname(Some(host))
        .tcp_port(port)
        .user(Some(conn.user.clone()))
        .pass(Some(conn.password.clone()))
        .prefer_socket(tunnel_port.is_none())
        .ssl_opts(ssl_opts(conn, tunnel_port.is_some()))
        .tcp_connect_timeout(Some(Duration::from_secs(3)));
    let pool = Pool::new(opts).map_err(|e| format!("cancel failed: {e}"))?;
    let mut c = pool
        .get_conn()
        .map_err(|e| format!("cancel failed: {e}"))?;
    c.query_drop(format!("KILL QUERY {conn_id}"))
        .map_err(|e| format!("cancel failed: {e}"))
}

/// Map server-side query interruption (KILL QUERY, error 1317) to a friendly message.
fn friendly_error(e: String) -> String {
    if e.contains("Query execution was interrupted") {
        "query cancelled".to_string()
    } else {
        e
    }
}

/// Server errors without the driver's `MySqlError { ... }` wrapper.
fn clean_error(e: &mysql::Error) -> String {
    match e {
        mysql::Error::MySqlError(e) => e.to_string(),
        other => other.to_string(),
    }
}

/// Build the driver TLS options for `conn`. `Some` makes TLS mandatory: the
/// driver refuses servers that do not advertise CLIENT_SSL.
///
/// Through an SSH tunnel the driver sees 127.0.0.1 as the peer, so hostname
/// verification can never succeed and is skipped; chain verification still applies.
fn ssl_opts(conn: &Connection, tunneled: bool) -> Option<mysql::SslOpts> {
    let tls = &conn.tls;
    let (verify_chain, verify_host) = match tls.mode {
        TlsMode::Off => return None,
        TlsMode::Required => (false, false),
        TlsMode::VerifyCa => (true, false),
        TlsMode::VerifyIdentity => (true, !tunneled),
    };
    let ca = tls.ca_path.trim();
    let ca_path = if ca.is_empty() {
        None
    } else {
        Some(std::borrow::Cow::Owned(expand_path(ca)))
    };
    Some(
        mysql::SslOpts::default()
            .with_root_cert_path(ca_path)
            .with_danger_accept_invalid_certs(!verify_chain)
            .with_danger_skip_domain_validation(!verify_host),
    )
}

/// A running `ssh -N -L` child process providing the tunnel.
struct Tunnel {
    child: std::process::Child,
    local_port: u16,
}

impl Drop for Tunnel {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Start an SSH local port-forward to `target_host:target_port` through the
/// jump host described by `ssh`, using the system `ssh` binary (agent and
/// key-based auth only, exactly like a manual `ssh -L`).
fn start_tunnel(ssh: &SshConfig, target_host: &str, target_port: u16) -> Result<Tunnel, String> {
    let listener = std::net::TcpListener::bind(("127.0.0.1", 0))
        .map_err(|e| format!("failed to reserve a local tunnel port: {e}"))?;
    let local_port = listener
        .local_addr()
        .map_err(|e| format!("failed to reserve a local tunnel port: {e}"))?
        .port();
    drop(listener);

    let dest = format!("127.0.0.1:{local_port}:{target_host}:{target_port}");
    let mut cmd = std::process::Command::new("ssh");
    cmd.arg("-N")
        .arg("-L")
        .arg(&dest)
        .arg("-o")
        .arg("ExitOnForwardFailure=yes")
        .arg("-o")
        .arg("ServerAliveInterval=30")
        .arg("-o")
        .arg("ServerAliveCountMax=3")
        .arg("-o")
        .arg("StrictHostKeyChecking=accept-new")
        .arg("-p")
        .arg(ssh.port.to_string());
    if !ssh.key_path.trim().is_empty() {
        cmd.arg("-i").arg(ssh.key_path.trim());
    }
    if !ssh.user.trim().is_empty() {
        cmd.arg(format!("{}@{}", ssh.user.trim(), ssh.host.trim()));
    } else {
        cmd.arg(ssh.host.trim());
    }
    cmd.stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    let mut child = cmd
        .spawn()
        .map_err(|e| format!("failed to start ssh (is the ssh client installed?): {e}"))?;

    // Wait until the local forward accepts connections (auth + forward setup).
    for _ in 0..150 {
        match child.try_wait() {
            Ok(Some(status)) => {
                return Err(format!("ssh exited during startup: {status}"));
            }
            Ok(None) => {}
            Err(e) => return Err(format!("failed to check ssh status: {e}")),
        }
        if std::net::TcpStream::connect(("127.0.0.1", local_port)).is_ok() {
            return Ok(Tunnel { child, local_port });
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    let _ = child.kill();
    let _ = child.wait();
    Err("ssh tunnel not ready after 15s (check host, user and key)".into())
}

struct WorkerState {
    conn: Option<PooledConn>,
    tunnel: Option<Tunnel>,
}

impl WorkerState {
    fn kill_tunnel(&mut self) {
        if let Some(t) = self.tunnel.take() {
            drop(t);
        }
    }
}

fn worker_loop(rx_cmd: Receiver<DbCommand>, tx_evt: Sender<DbEvent>) {
    let mut state = WorkerState {
        conn: None,
        tunnel: None,
    };
    while let Ok(cmd) = rx_cmd.recv() {
        match cmd {
            DbCommand::Connect(conn) => {
                state.kill_tunnel();
                state.conn = None;
                let mut tunnel_port = None;
                if conn.has_ssh() {
                    let ssh = conn.ssh.clone().unwrap_or_default();
                    match start_tunnel(&ssh, &conn.host, conn.port) {
                        Ok(t) => {
                            tunnel_port = Some(t.local_port);
                            // the child must outlive the connect attempt
                            state.tunnel = Some(t);
                        }
                        Err(e) => {
                            let _ = tx_evt.send(DbEvent::ConnectFailed(e));
                            continue;
                        }
                    }
                }
                match connect(&conn, tunnel_port) {
                    Ok((c, info, schema)) => {
                        state.conn = Some(c);
                        let _ = tx_evt.send(DbEvent::Connected(info));
                        let _ = tx_evt.send(DbEvent::SchemaUpdated(schema));
                    }
                    Err(e) => {
                        state.kill_tunnel();
                        let _ = tx_evt.send(DbEvent::ConnectFailed(e));
                    }
                }
            }
            DbCommand::Disconnect => {
                state.conn = None;
                state.kill_tunnel();
                let _ = tx_evt.send(DbEvent::Disconnected);
            }
            DbCommand::Query(sql) => {
                let Some(conn) = state.conn.as_mut() else {
                    let _ = tx_evt.send(DbEvent::QueryFailed("not connected".into()));
                    continue;
                };
                let started = Instant::now();
                match run_query(conn, &sql) {
                    Ok((results, message, schema_changed)) => {
                        let elapsed = started.elapsed();
                        let results = results
                            .into_iter()
                            .map(|mut r| {
                                r.elapsed = elapsed;
                                r
                            })
                            .collect();
                        let _ = tx_evt.send(DbEvent::QueryDone { results, message });
                        if schema_changed && let Ok(schema) = fetch_schema(conn) {
                            let _ = tx_evt.send(DbEvent::SchemaUpdated(schema));
                        }
                    }
                    Err(e) => {
                        let _ = tx_evt.send(DbEvent::QueryFailed(friendly_error(e)));
                    }
                }
            }
            DbCommand::QueryPage { sql, page } => {
                let Some(conn) = state.conn.as_mut() else {
                    let _ = tx_evt.send(DbEvent::PageFailed("not connected".into()));
                    continue;
                };
                let started = Instant::now();
                match run_page(conn, &sql) {
                    Ok(rows) => {
                        let has_more = rows.len() >= page;
                        let _ = tx_evt.send(DbEvent::PageDone {
                            rows,
                            has_more,
                            elapsed: started.elapsed(),
                        });
                    }
                    Err(e) => {
                        let _ = tx_evt.send(DbEvent::PageFailed(friendly_error(e)));
                    }
                }
            }
            DbCommand::RefreshSchema => match state.conn.as_mut() {
                Some(conn) => match fetch_schema(conn) {
                    Ok(schema) => {
                        let _ = tx_evt.send(DbEvent::SchemaUpdated(schema));
                    }
                    Err(e) => {
                        let _ = tx_evt
                            .send(DbEvent::QueryFailed(format!("schema refresh failed: {e}")));
                    }
                },
                None => {
                    let _ = tx_evt.send(DbEvent::QueryFailed("not connected".into()));
                }
            },
            DbCommand::FetchTableDetail(database, table) => match state.conn.as_mut() {
                Some(conn) => match fetch_table_detail(conn, &database, &table) {
                    Ok((columns, ddl)) => {
                        let _ = tx_evt.send(DbEvent::TableDetailFetched {
                            database,
                            table,
                            columns,
                            ddl,
                        });
                    }
                    Err(e) => {
                        let _ = tx_evt.send(DbEvent::QueryFailed(e));
                    }
                },
                None => {
                    let _ = tx_evt.send(DbEvent::QueryFailed("not connected".into()));
                }
            },
            DbCommand::ExportAll { sql, path, format } => {
                let Some(conn) = state.conn.as_mut() else {
                    let _ = tx_evt.send(DbEvent::ExportDone {
                        result: Err("not connected".into()),
                    });
                    continue;
                };
                run_paginated_export(conn, &sql, &path, format, &tx_evt);
            }
        }
    }
}

fn connect(conn: &Connection, tunnel_port: Option<u16>) -> Result<(PooledConn, ConnInfo, SchemaCache), String> {
    // Through a tunnel the MySQL endpoint is the local forwarded port.
    let (host, port) = match tunnel_port {
        Some(p) => ("127.0.0.1".to_string(), p),
        None => (conn.host.clone(), conn.port),
    };
    let opts = mysql::OptsBuilder::new()
        .ip_or_hostname(Some(host))
        .tcp_port(port)
        .user(Some(conn.user.clone()))
        .pass(Some(conn.password.clone()))
        .db_name(if conn.database.is_empty() {
            None
        } else {
            Some(conn.database.clone())
        })
        .prefer_socket(tunnel_port.is_none())
        .ssl_opts(ssl_opts(conn, tunnel_port.is_some()))
        .tcp_connect_timeout(Some(Duration::from_secs(8)));
    let pool = Pool::new(opts).map_err(|e| format!("connection failed: {e}"))?;
    let mut c = pool
        .get_conn()
        .map_err(|e| format!("connection failed: {e}"))?;

    let version: String = c
        .query_first("SELECT VERSION()")
        .ok()
        .flatten()
        .unwrap_or_else(|| "unknown".to_string());
    let database: Option<String> = c
        .query_first("SELECT COALESCE(DATABASE(), '')")
        .map_err(|e| format!("failed to read current database: {e}"))?;
    let conn_id: u64 = c
        .query_first("SELECT CONNECTION_ID()")
        .map_err(|e| format!("failed to read connection id: {e}"))?
        .ok_or("failed to read connection id")?;
    let ssl_cipher: Option<String> = c
        .query_first::<(String, String), _>("SHOW SESSION STATUS LIKE 'Ssl_cipher'")
        .ok()
        .flatten()
        .map(|(_, v)| v)
        .filter(|v| !v.is_empty());
    if conn.has_tls() && ssl_cipher.is_none() {
        return Err("TLS was requested but the session is not encrypted".into());
    }
    let info = ConnInfo {
        server_version: version,
        database: database.unwrap_or_default(),
        user: conn.user.clone(),
        host: conn.host.clone(),
        port: conn.port,
        conn_id,
        tunnel_port,
        ssl_cipher,
    };
    let schema = fetch_schema(&mut c)?;
    Ok((c, info, schema))
}

fn fetch_schema(conn: &mut PooledConn) -> Result<SchemaCache, String> {
    let database: Option<String> = conn
        .query_first("SELECT COALESCE(DATABASE(), '')")
        .map_err(|e| e.to_string())?;

    let mut tables = Vec::new();
    let mut columns: std::collections::HashMap<String, Vec<String>> =
        std::collections::HashMap::new();
    if let Some(db) = database.as_ref() {
        let rows: Vec<(String, String)> = conn
            .exec(
                "SELECT TABLE_NAME, COLUMN_NAME \
                 FROM information_schema.COLUMNS \
                 WHERE TABLE_SCHEMA = ? \
                 ORDER BY TABLE_NAME, ORDINAL_POSITION",
                (db.as_str(),),
            )
            .map_err(|e| e.to_string())?;
        for (table, column) in rows {
            if !tables.last().is_some_and(|t| t == &table) {
                tables.push(table.clone());
            }
            columns
                .entry(table.to_lowercase())
                .or_default()
                .push(column);
        }
    }

    let databases: Vec<String> = conn
        .query("SELECT SCHEMA_NAME FROM information_schema.SCHEMATA ORDER BY SCHEMA_NAME")
        .map_err(|e| e.to_string())?;

    let mut all_columns: Vec<String> = columns.values().flatten().cloned().collect();
    all_columns.sort();
    all_columns.dedup();

    let mut tables_by_db: std::collections::HashMap<String, Vec<String>> =
        std::collections::HashMap::new();
    let table_rows: Vec<(String, String)> = conn
        .query(
            "SELECT TABLE_SCHEMA, TABLE_NAME \
             FROM information_schema.TABLES \
             WHERE TABLE_TYPE = 'BASE TABLE' \
             ORDER BY TABLE_SCHEMA, TABLE_NAME",
        )
        .map_err(|e| e.to_string())?;
    for (db, table) in table_rows {
        tables_by_db.entry(db).or_default().push(table);
    }

    Ok(SchemaCache {
        database: database.unwrap_or_default(),
        tables,
        columns,
        all_columns,
        databases,
        tables_by_db,
        column_names: std::collections::HashMap::new(),
        ddl: std::collections::HashMap::new(),
    })
}

/// Fetch column names and DDL for a specific table.
fn fetch_table_detail(
    conn: &mut PooledConn,
    database: &str,
    table: &str,
) -> Result<(Vec<String>, String), String> {
    let current_db: Option<String> = conn
        .query_first("SELECT DATABASE()")
        .map_err(|e| e.to_string())?;

    if current_db.as_deref() != Some(database) {
        conn.query_drop(format!("USE `{database}`"))
            .map_err(|e| format!("failed to switch to database {database}: {e}"))?;
    }

    let columns: Vec<String> = conn
        .query(format!(
            "SELECT COLUMN_NAME FROM INFORMATION_SCHEMA.COLUMNS \
             WHERE TABLE_SCHEMA = '{database}' AND TABLE_NAME = '{table}' \
             ORDER BY ORDINAL_POSITION"
        ))
        .map_err(|e| format!("failed to fetch columns for {database}.{table}: {e}"))?;

    let ddl: String = conn
        .query_first(format!("SHOW CREATE TABLE `{table}`"))
        .map_err(|e| format!("failed to fetch DDL for {database}.{table}: {e}"))?
        .map(|(_, create_sql): (String, String)| create_sql)
        .unwrap_or_default();

    if let Some(orig_db) = current_db
        && orig_db != database
    {
        conn.query_drop(format!("USE `{orig_db}`"))
            .map_err(|e| format!("failed to restore database {orig_db}: {e}"))?;
    }

    Ok((columns, ddl))
}

/// Fetch all rows by paginating with LIMIT/OFFSET, writing incrementally to a file.
fn run_paginated_export(
    conn: &mut PooledConn,
    sql: &str,
    path: &str,
    format: ExportFormat,
    tx_evt: &Sender<DbEvent>,
) {
    let statements = split_statements(sql);
    if statements.is_empty() {
        let _ = tx_evt.send(DbEvent::ExportDone {
            result: Err("empty query".into()),
        });
        return;
    }
    let sql = statements.last().unwrap();
    let upper = sql.trim_start().to_uppercase();
    if !upper.starts_with("SELECT") {
        let _ = tx_evt.send(DbEvent::ExportDone {
            result: Err("only SELECT queries can be exported".into()),
        });
        return;
    }

    let out_path = with_default_ext(&expand_path(path), format);
    let page_size = MAX_ROWS;
    let mut offset = 0usize;
    let mut writer: Option<PageWriter<File>> = None;

    loop {
        let page_sql = format!("{sql} LIMIT {page_size} OFFSET {offset}");
        let rs = match conn.query_iter(&page_sql) {
            Ok(rs) => rs,
            Err(e) => {
                let _ = tx_evt.send(DbEvent::ExportDone {
                    result: Err(friendly_error(format!("{e} (offset={offset})"))),
                });
                return;
            }
        };

        let columns: Vec<String> = rs
            .columns()
            .as_ref()
            .iter()
            .map(|c| c.name_str().to_string())
            .collect();

        let mut rows: Vec<Vec<Value>> = Vec::new();
        for row in rs {
            match row {
                Ok(r) => rows.push(row_to_values(r)),
                Err(e) => {
                    let _ = tx_evt.send(DbEvent::ExportDone {
                        result: Err(format!("row read error: {e}")),
                    });
                    return;
                }
            }
        }

        if rows.is_empty() {
            break;
        }

        if writer.is_none() {
            match PageWriter::open(out_path.clone(), format, &columns) {
                Ok(w) => writer = Some(w),
                Err(e) => {
                    let _ = tx_evt.send(DbEvent::ExportDone {
                        result: Err(e),
                    });
                    return;
                }
            }
        }

        let n = rows.len();
        if let Some(w) = writer.as_mut()
            && let Err(e) = w.export_page(&columns, &rows)
        {
            let _ = tx_evt.send(DbEvent::ExportDone {
                result: Err(e),
            });
            return;
        }
        offset += n;
        let _ = tx_evt.send(DbEvent::ExportProgress(offset));

        if n < page_size {
            break;
        }
    }

    match writer {
        Some(w) => {
            let _ = tx_evt.send(DbEvent::ExportDone {
                result: w.finish(),
            });
        }
        None => {
            let _ = tx_evt.send(DbEvent::ExportDone {
                result: Ok((out_path, 0)),
            });
        }
    }
}

fn row_to_values(row: mysql::Row) -> Vec<Value> {
    (0..row.len()).map(|i| row.as_ref(i).cloned().unwrap_or(Value::NULL)).collect()
}

/// Run possibly multiple statements; return every result set that had
/// columns, plus a summary message for statements that only affected rows.
fn run_query(conn: &mut PooledConn, sql: &str) -> Result<(Vec<QueryResult>, String, bool), String> {
    let statements = split_statements(sql);
    if statements.is_empty() {
        return Err("empty query".into());
    }
    let mut results: Vec<QueryResult> = Vec::new();
    let mut total_affected: u64 = 0;
    let mut schema_changed = false;

    for stmt in statements {
        let stmt_upper = stmt.trim_start().to_uppercase();
        if stmt_upper.starts_with("CREATE")
            || stmt_upper.starts_with("DROP")
            || stmt_upper.starts_with("ALTER")
            || stmt_upper.starts_with("USE ")
        {
            schema_changed = true;
        }
        let result_set = conn
            .query_iter(&stmt)
            .map_err(|e| format!("{}\n\nin: {}", clean_error(&e), truncate(&stmt, 200)))?;
        let columns: Vec<String> = result_set
            .columns()
            .as_ref()
            .iter()
            .map(|c| c.name_str().to_string())
            .collect();
        if columns.is_empty() {
            // DML/DDL: no result set
            drop(result_set);
            total_affected += conn.affected_rows();
            continue;
        }
        let mut rows = Vec::new();
        let mut truncated = false;
        for row in result_set {
            let row = row.map_err(|e| clean_error(&e))?;
            if rows.len() >= MAX_ROWS {
                truncated = true;
                break;
            }
            rows.push(row.unwrap());
        }
        let source = is_pageable(&stmt).then(|| stmt.trim().to_string());
        let has_more = source.is_some()
            && (truncated || split_limit(&stmt).2.is_some_and(|l| l > 0 && rows.len() >= l));
        results.push(QueryResult {
            columns,
            rows,
            truncated,
            elapsed: Duration::ZERO,
            source,
            has_more,
        });
    }

    let message = if results.is_empty() {
        format!(
            "{total_affected} row{} affected",
            if total_affected == 1 { "" } else { "s" }
        )
    } else {
        let total_rows: usize = results.iter().map(|r| r.rows.len()).sum();
        let truncated = results.iter().any(|r| r.truncated);
        let mut msg = if results.len() == 1 {
            if truncated {
                format!("{total_rows}+ rows (truncated at {MAX_ROWS})")
            } else {
                format!("{total_rows} row{}", if total_rows == 1 { "" } else { "s" })
            }
        } else if truncated {
            format!(
                "{total_rows} rows in {} result sets (each truncated at {MAX_ROWS})",
                results.len()
            )
        } else {
            format!("{total_rows} rows in {} result sets", results.len())
        };
        if total_affected > 0 {
            msg.push_str(&format!(
                "; {total_affected} row{} affected",
                if total_affected == 1 { "" } else { "s" }
            ));
        }
        msg
    };
    Ok((results, message, schema_changed))
}

/// Fetch every row of one already-paged SELECT.
fn run_page(conn: &mut PooledConn, sql: &str) -> Result<Vec<Vec<Value>>, String> {
    let result_set = conn.query_iter(sql).map_err(|e| clean_error(&e))?;
    let mut rows = Vec::new();
    for row in result_set {
        let row = row.map_err(|e| clean_error(&e))?;
        if rows.len() >= MAX_ROWS {
            break;
        }
        rows.push(row_to_values(row));
    }
    Ok(rows)
}

/// Statements whose result can be extended by appending LIMIT/OFFSET.
fn is_pageable(stmt: &str) -> bool {
    let upper = stmt.trim_start().to_ascii_uppercase();
    upper.starts_with("SELECT") || upper.starts_with("WITH")
}

/// Split a trailing `LIMIT n`, `LIMIT o, n` or `LIMIT n OFFSET o` off `stmt`.
/// Returns the statement without that clause, the offset and the row count;
/// a statement without a parseable trailing LIMIT is returned whole.
pub fn split_limit(stmt: &str) -> (&str, usize, Option<usize>) {
    let s = stmt.trim().trim_end_matches(';').trim_end();
    let upper = s.to_ascii_uppercase();
    let Some(pos) = upper.rfind("LIMIT") else {
        return (s, 0, None);
    };
    let before_ok = pos == 0
        || upper[..pos].ends_with(|c: char| c.is_whitespace() || c == ')');
    let tail = &s[pos + "LIMIT".len()..];
    if !before_ok || !tail.starts_with(|c: char| c.is_whitespace()) {
        return (s, 0, None);
    }
    let spaced = tail.replace(',', " , ");
    let tokens: Vec<&str> = spaced.split_whitespace().collect();
    let num = |t: &str| t.parse::<usize>().ok();
    let parsed = match tokens.as_slice() {
        [n] => num(n).map(|n| (0, n)),
        [o, ",", n] => num(o).zip(num(n)),
        [n, kw, o] if kw.eq_ignore_ascii_case("OFFSET") => num(o).zip(num(n)),
        _ => None,
    };
    match parsed {
        Some((offset, limit)) => (s[..pos].trim_end(), offset, Some(limit)),
        None => (s, 0, None),
    }
}

/// SQL fetching the batch that follows `fetched` rows of `source`, plus the
/// batch size: the statement's own LIMIT (so an auto-applied default LIMIT
/// pages by that size) capped at MAX_ROWS. None only for `LIMIT 0`.
pub fn next_page_sql(source: &str, fetched: usize) -> Option<(String, usize)> {
    let (base, offset, limit) = split_limit(source);
    let page = limit.unwrap_or(MAX_ROWS).min(MAX_ROWS);
    if page == 0 {
        return None;
    }
    Some((
        format!("{base} LIMIT {page} OFFSET {}", offset + fetched),
        page,
    ))
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let t: String = s.chars().take(max).collect();
        format!("{t}...")
    }
}

/// Split SQL text into statements on ';', respecting quotes and comments.
pub fn split_statements(sql: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut current = String::new();
    let mut chars = sql.chars().peekable();
    let mut quote: Option<char> = None;
    let mut in_line_comment = false;
    let mut in_block_comment = false;

    while let Some(c) = chars.next() {
        if in_line_comment {
            current.push(c);
            if c == '\n' {
                in_line_comment = false;
            }
            continue;
        }
        if in_block_comment {
            current.push(c);
            if c == '*' && chars.peek() == Some(&'/') {
                current.push(chars.next().unwrap());
                in_block_comment = false;
            }
            continue;
        }
        if let Some(q) = quote {
            current.push(c);
            if c == '\\' && (q == '\'' || q == '"') {
                if let Some(&next) = chars.peek() {
                    current.push(next);
                    chars.next();
                }
            } else if c == q {
                quote = None;
            }
            continue;
        }
        match c {
            '\'' | '"' | '`' => {
                quote = Some(c);
                current.push(c);
            }
            '-' => {
                current.push(c);
                if chars.peek() == Some(&'-') {
                    current.push(chars.next().unwrap());
                    in_line_comment = true;
                }
            }
            '/' => {
                current.push(c);
                if chars.peek() == Some(&'*') {
                    current.push(chars.next().unwrap());
                    in_block_comment = true;
                }
            }
            ';' => {
                let stmt = current.trim().to_string();
                if !stmt.is_empty() {
                    out.push(stmt);
                }
                current.clear();
            }
            _ => current.push(c),
        }
    }
    let stmt = current.trim().to_string();
    if !stmt.is_empty() {
        out.push(stmt);
    }
    out
}

/// Render a Value for on-screen display.
pub fn value_to_display(v: &Value) -> String {
    match v {
        Value::NULL => "NULL".to_string(),
        Value::Bytes(b) => String::from_utf8_lossy(b).to_string(),
        Value::Int(i) => i.to_string(),
        Value::UInt(u) => u.to_string(),
        Value::Float(f) => f.to_string(),
        Value::Double(d) => d.to_string(),
        Value::Date(y, mo, d, h, mi, s, _us) => {
            format!("{y:04}-{mo:02}-{d:02} {h:02}:{mi:02}:{s:02}")
        }
        Value::Time(neg, d, h, mi, s, _us) => {
            format!("{}{d}d {h:02}:{mi:02}:{s:02}", if *neg { "-" } else { "" })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clean_error_strips_driver_wrapper() {
        let e = mysql::Error::MySqlError(mysql::error::MySqlError {
            state: "42000".into(),
            message: "You have an error in your SQL syntax".into(),
            code: 1064,
        });
        let s = clean_error(&e);
        assert_eq!(s, "ERROR 1064 (42000): You have an error in your SQL syntax");
        assert!(!s.contains("MySqlError {"));
    }

    #[test]
    fn ssl_opts_follow_mode_and_tunnel() {
        use crate::config::TlsConfig;
        let mk = |mode| Connection {
            tls: TlsConfig {
                mode,
                ca_path: String::new(),
            },
            ..Connection::default()
        };
        assert!(ssl_opts(&mk(TlsMode::Off), false).is_none());

        let o = ssl_opts(&mk(TlsMode::Required), false).unwrap();
        assert!(o.accept_invalid_certs() && o.skip_domain_validation());

        let o = ssl_opts(&mk(TlsMode::VerifyCa), false).unwrap();
        assert!(!o.accept_invalid_certs() && o.skip_domain_validation());

        let o = ssl_opts(&mk(TlsMode::VerifyIdentity), false).unwrap();
        assert!(!o.accept_invalid_certs() && !o.skip_domain_validation());

        // hostname check is impossible through a tunnel (peer is 127.0.0.1)
        let o = ssl_opts(&mk(TlsMode::VerifyIdentity), true).unwrap();
        assert!(!o.accept_invalid_certs() && o.skip_domain_validation());

        let mut c = mk(TlsMode::VerifyCa);
        c.tls.ca_path = "~/ca.pem".into();
        let o = ssl_opts(&c, false).unwrap();
        assert!(o.root_cert_path().unwrap().ends_with("ca.pem"));
        assert!(!o.root_cert_path().unwrap().starts_with("~"));
    }

    /// Probe against a live server at 127.0.0.1:3306 with bogus credentials.
    /// The TLS handshake precedes authentication, so "Access denied" proves
    /// the encrypted channel was established; a certificate error under
    /// verify-identity proves verification is enforced.
    #[test]
    #[ignore]
    fn tls_probe_live_local_server() {
        use crate::config::TlsConfig;
        let mk = |mode| Connection {
            user: "tls_probe_nobody".into(),
            password: "x".into(),
            tls: TlsConfig {
                mode,
                ca_path: String::new(),
            },
            ..Connection::default()
        };
        for mode in TlsMode::ALL {
            let err = connect(&mk(mode), None).err().unwrap_or_default();
            eprintln!("{:<16} -> {err}", mode.as_str());
        }
    }

    #[test]
    fn split_simple() {
        let out = split_statements("SELECT 1; SELECT 2;");
        assert_eq!(out, vec!["SELECT 1", "SELECT 2"]);
    }

    #[test]
    fn split_keeps_trailing_statement_without_semicolon() {
        let out = split_statements("SELECT 1;\nSELECT 2");
        assert_eq!(out, vec!["SELECT 1", "SELECT 2"]);
    }

    #[test]
    fn split_respects_quotes() {
        let out = split_statements("SELECT 'a;b' AS x; SELECT `t;1`.c");
        assert_eq!(out, vec!["SELECT 'a;b' AS x", "SELECT `t;1`.c"]);
    }

    #[test]
    fn split_respects_comments() {
        let out = split_statements("SELECT 1 -- ; not a separator;\n; SELECT /* ; */ 2");
        assert_eq!(out.len(), 2);
        assert!(out[1].contains("/* ; */"));
    }

    #[test]
    fn split_skips_empty_statements() {
        let out = split_statements(";;  ;\n;;");
        assert!(out.is_empty());
    }

    #[test]
    fn split_escaped_quote() {
        let out = split_statements("SELECT 'it''s;ok'");
        assert_eq!(out, vec!["SELECT 'it''s;ok'"]);
    }

    #[test]
    fn friendly_error_maps_interruption() {
        assert_eq!(
            friendly_error("Query execution was interrupted (offset=5000)".into()),
            "query cancelled"
        );
        assert_eq!(friendly_error("syntax error".into()), "syntax error");
    }

    #[test]
    fn display_values() {
        assert_eq!(value_to_display(&Value::NULL), "NULL");
        assert_eq!(value_to_display(&Value::Int(-5)), "-5");
        assert_eq!(value_to_display(&Value::UInt(5)), "5");
        assert_eq!(value_to_display(&Value::Bytes(b"txt".to_vec())), "txt");
        assert_eq!(
            value_to_display(&Value::Date(2026, 9, 11, 10, 30, 0, 0)),
            "2026-09-11 10:30:00"
        );
    }

    #[test]
    fn split_limit_parses_trailing_forms() {
        assert_eq!(split_limit("SELECT * FROM t"), ("SELECT * FROM t", 0, None));
        assert_eq!(
            split_limit("SELECT * FROM t LIMIT 1000;"),
            ("SELECT * FROM t", 0, Some(1000))
        );
        assert_eq!(
            split_limit("select * from t limit 5, 10"),
            ("select * from t", 5, Some(10))
        );
        assert_eq!(
            split_limit("SELECT * FROM t\nLIMIT 10 OFFSET 20"),
            ("SELECT * FROM t", 20, Some(10))
        );
        // a LIMIT inside a subquery is not a trailing clause
        let sub = "SELECT * FROM (SELECT id FROM t LIMIT 5) x";
        assert_eq!(split_limit(sub), (sub, 0, None));
        // a column called `limit_x` must not be mistaken for the keyword
        let col = "SELECT limit_x FROM t";
        assert_eq!(split_limit(col), (col, 0, None));
    }

    #[test]
    fn next_page_sql_pages_by_own_limit_or_max_rows() {
        assert_eq!(
            next_page_sql("SELECT * FROM t LIMIT 1000", 1000),
            Some(("SELECT * FROM t LIMIT 1000 OFFSET 1000".into(), 1000))
        );
        assert_eq!(
            next_page_sql("SELECT * FROM t LIMIT 5, 10", 10),
            Some(("SELECT * FROM t LIMIT 10 OFFSET 15".into(), 10))
        );
        assert_eq!(
            next_page_sql("SELECT * FROM t", MAX_ROWS),
            Some((
                format!("SELECT * FROM t LIMIT {MAX_ROWS} OFFSET {MAX_ROWS}"),
                MAX_ROWS
            ))
        );
        assert_eq!(
            next_page_sql("SELECT * FROM t LIMIT 50000", MAX_ROWS).map(|p| p.1),
            Some(MAX_ROWS)
        );
        assert_eq!(next_page_sql("SELECT * FROM t LIMIT 0", 0), None);
    }

    #[test]
    fn query_result_next_page_needs_a_source() {
        let mut r = QueryResult {
            columns: vec!["a".into()],
            rows: vec![vec![Value::Int(1)]; 3],
            truncated: false,
            elapsed: Duration::ZERO,
            source: None,
            has_more: false,
        };
        assert!(r.next_page().is_none());
        r.source = Some("SELECT a FROM t LIMIT 3".into());
        assert_eq!(
            r.next_page(),
            Some(("SELECT a FROM t LIMIT 3 OFFSET 3".into(), 3))
        );
        assert!(is_pageable("  with x as (select 1) select * from x"));
        assert!(!is_pageable("SHOW TABLES"));
    }
}
