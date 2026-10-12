use crate::completer::{Completer, Suggestion};
use crate::config::{Config, Connection, SshConfig, TlsConfig, TlsMode};
use crate::db::{ConnInfo, DbCommand, DbEvent, DbWorker, QueryResult, value_to_display};
use mysql::Value;
use crate::editor::Editor;
use crate::exporter::{ExportFormat, export};
use crate::ui;
use std::collections::HashSet;
use crossterm::event::{
    self, DisableBracketedPaste, EnableBracketedPaste, Event, KeyCode, KeyEvent, KeyEventKind,
    KeyModifiers,
};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::widgets::TableState;
use unicode_width::UnicodeWidthStr;
use std::io;
use std::time::Duration;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Password,
    ConnList,
    ConnForm,
    Query,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PasswordMode {
    /// First run or plaintext config — user sets a new password (with confirmation).
    Setup,
    /// Encrypted config exists — user enters password to unlock.
    Unlock,
}

pub struct PasswordState {
    pub mode: PasswordMode,
    pub password: String,
    pub confirm: String,
    /// Index of the active field: 0 = password, 1 = confirm (setup mode only).
    pub active: usize,
    /// B64-encoded salt and encrypted payload for Unlock mode.
    pub salt: String,
    pub nonce: String,
    pub ciphertext: String,
    /// Error message shown to the user.
    pub error: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    Editor,
    Results,
    Sidebar,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StatusKind {
    Info,
    Ok,
    Error,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Busy {
    Connecting,
    Querying,
    Exporting,
}

pub struct PopupState {
    pub suggestions: Vec<Suggestion>,
    pub selected: usize,
}

pub struct FormField {
    pub label: &'static str,
    pub value: String,
    pub secret: bool,
    pub cursor: usize,
    /// When set, the field is a fixed-choice selector cycled with Left/Right.
    pub choices: Option<&'static [&'static str]>,
}

impl FormField {
    fn new(label: &'static str, value: String, secret: bool) -> Self {
        let cursor = value.len();
        Self {
            label,
            value,
            secret,
            cursor,
            choices: None,
        }
    }

    fn choice(label: &'static str, choices: &'static [&'static str], value: &str) -> Self {
        let value = if choices.contains(&value) { value } else { choices[0] };
        Self {
            label,
            value: value.to_string(),
            secret: false,
            cursor: 0,
            choices: Some(choices),
        }
    }

    pub fn is_choice(&self) -> bool {
        self.choices.is_some()
    }

    fn cycle(&mut self, step: isize) {
        let Some(choices) = self.choices else { return };
        let n = choices.len() as isize;
        let cur = choices.iter().position(|c| *c == self.value).unwrap_or(0) as isize;
        self.value = choices[((cur + step).rem_euclid(n)) as usize].to_string();
    }

    pub fn rendered(&self) -> String {
        if self.secret {
            "*".repeat(self.value.chars().count())
        } else {
            self.value.clone()
        }
    }

    fn insert(&mut self, c: char) {
        if self.is_choice() {
            return;
        }
        if self.cursor > self.value.len() {
            self.cursor = self.value.len();
        }
        self.value.insert(self.cursor, c);
        self.cursor += c.len_utf8();
    }

    fn backspace(&mut self) {
        if self.is_choice() {
            return;
        }
        if self.cursor > 0 {
            let mut cut = self.cursor - 1;
            while cut > 0 && !self.value.is_char_boundary(cut) {
                cut -= 1;
            }
            self.value.replace_range(cut..self.cursor, "");
            self.cursor = cut;
        }
    }

    fn delete(&mut self) {
        if self.is_choice() {
            return;
        }
        if self.cursor < self.value.len() {
            let mut end = self.cursor + 1;
            while end < self.value.len() && !self.value.is_char_boundary(end) {
                end += 1;
            }
            self.value.replace_range(self.cursor..end, "");
        }
    }

    fn left(&mut self) {
        if self.is_choice() {
            self.cycle(-1);
            return;
        }
        while self.cursor > 0 && !self.value.is_char_boundary(self.cursor - 1) {
            self.cursor -= 1;
        }
        if self.cursor > 0 {
            self.cursor -= 1;
        }
    }

    fn right(&mut self) {
        if self.is_choice() {
            self.cycle(1);
            return;
        }
        if self.cursor < self.value.len() {
            self.cursor += 1;
            while self.cursor < self.value.len() && !self.value.is_char_boundary(self.cursor) {
                self.cursor += 1;
            }
        }
    }
}

pub struct ConnForm {
    /// Index of the connection being edited; None when creating a new one.
    pub index: Option<usize>,
    pub fields: Vec<FormField>,
    pub active: usize,
}

const TLS_CHOICES: &[&str] = &["off", "required", "verify-ca", "verify-identity"];

impl ConnForm {
    pub fn from_connection(index: Option<usize>, c: &Connection) -> Self {
        let ssh = c.ssh.clone().unwrap_or_default();
        Self {
            index,
            fields: vec![
                FormField::new("Name", c.name.clone(), false),
                FormField::new("Host", c.host.clone(), false),
                FormField::new("Port", c.port.to_string(), false),
                FormField::new("User", c.user.clone(), false),
                FormField::new("Password", c.password.clone(), true),
                FormField::new("Database", c.database.clone(), false),
                FormField::new("SSH Host", ssh.host.clone(), false),
                FormField::new("SSH Port", ssh.port.to_string(), false),
                FormField::new("SSH User", ssh.user.clone(), false),
                FormField::new("SSH Key", ssh.key_path.clone(), false),
                FormField::choice("TLS", TLS_CHOICES, c.tls.mode.as_str()),
                FormField::new("TLS CA", c.tls.ca_path.clone(), false),
            ],
            active: 0,
        }
    }

    fn to_connection(&self) -> Result<Connection, String> {
        let port: u16 = self.fields[2]
            .value
            .trim()
            .parse()
            .map_err(|_| "port must be a number between 1 and 65535".to_string())?;
        // An empty SSH Host means a direct connection.
        let ssh = if self.fields[6].value.trim().is_empty() {
            None
        } else {
            let port_str = self.fields[7].value.trim();
            let port: u16 = if port_str.is_empty() {
                22
            } else {
                port_str.parse().map_err(|_| {
                    "ssh port must be a number between 1 and 65535".to_string()
                })?
            };
            Some(SshConfig {
                host: self.fields[6].value.trim().to_string(),
                port,
                user: self.fields[8].value.trim().to_string(),
                key_path: self.fields[9].value.trim().to_string(),
            })
        };
        let tls_mode = TlsMode::parse(&self.fields[10].value)
            .ok_or_else(|| format!("unknown TLS mode '{}'", self.fields[10].value))?;
        let ca_path = self.fields[11].value.trim().to_string();
        if tls_mode == TlsMode::Off && !ca_path.is_empty() {
            return Err("TLS CA is set but TLS is off".to_string());
        }
        Ok(Connection {
            name: self.fields[0].value.trim().to_string(),
            host: if self.fields[1].value.trim().is_empty() {
                "127.0.0.1".to_string()
            } else {
                self.fields[1].value.trim().to_string()
            },
            port,
            user: if self.fields[3].value.trim().is_empty() {
                "root".to_string()
            } else {
                self.fields[3].value.trim().to_string()
            },
            password: self.fields[4].value.clone(),
            database: self.fields[5].value.trim().to_string(),
            ssh,
            tls: TlsConfig {
                mode: tls_mode,
                ca_path,
            },
        })
    }
}

pub struct ExportDialog {
    pub path: String,
    pub cursor: usize,
    pub format: ExportFormat,
    /// 0 = path field, 1 = format selector
    pub active: usize,
    /// True when the in-memory result was truncated; Enter triggers paginated full export.
    pub truncate: bool,
}

pub enum ConfirmAction {
    DeleteConnection(usize),
    RunDestructive(String),
}

pub struct Confirm {
    pub message: String,
    pub action: ConfirmAction,
}

/// Cached render-friendly view of a query result.
pub struct ResultView {
    pub result: QueryResult,
    pub display: Vec<Vec<String>>,
    pub widths: Vec<u16>,
}

const MAX_COL_WIDTH: u16 = 40;

impl ResultView {
    pub fn new(result: QueryResult) -> Self {
        let display: Vec<Vec<String>> = result
            .rows
            .iter()
            .map(|r| r.iter().map(value_to_display).collect())
            .collect();
        let mut widths = Vec::with_capacity(result.columns.len());
        for (ci, col) in result.columns.iter().enumerate() {
            let mut w = col.width() as u16;
            for row in &display {
                let len = row.get(ci).map(|s| cell_width(s) as u16).unwrap_or(0);
                w = w.max(len);
            }
            widths.push(w.clamp(4, MAX_COL_WIDTH));
        }
        Self {
            result,
            display,
            widths,
        }
    }

    /// Extend the view with the next batch of rows fetched for the same statement.
    pub fn append(&mut self, rows: Vec<Vec<Value>>, has_more: bool) {
        for row in &rows {
            let disp: Vec<String> = row.iter().map(value_to_display).collect();
            for (ci, w) in self.widths.iter_mut().enumerate() {
                let len = disp.get(ci).map(|s| cell_width(s) as u16).unwrap_or(0);
                *w = (*w).max(len).min(MAX_COL_WIDTH);
            }
            self.display.push(disp);
        }
        self.result.rows.extend(rows);
        self.result.has_more = has_more;
        self.result.truncated = has_more;
    }
}

/// Display width of the first line of a cell, which is all the grid shows.
fn cell_width(s: &str) -> usize {
    s.lines().next().unwrap_or("").width()
}

/// Full-content popup for one result cell.
pub struct CellView {
    pub column: String,
    pub row: usize,
    pub raw: String,
    /// Pretty-printed form when the value parses as a JSON object/array.
    pub pretty: Option<String>,
    pub show_pretty: bool,
    pub scroll: usize,
    /// Visible body height, recorded by the renderer for page scrolling.
    pub page: usize,
}

impl CellView {
    pub fn new(column: String, row: usize, raw: String) -> Self {
        let pretty = pretty_json(&raw);
        Self {
            column,
            row,
            raw,
            show_pretty: pretty.is_some(),
            pretty,
            scroll: 0,
            page: 1,
        }
    }

    pub fn text(&self) -> &str {
        match (&self.pretty, self.show_pretty) {
            (Some(p), true) => p,
            _ => &self.raw,
        }
    }

    pub fn toggle_pretty(&mut self) -> bool {
        if self.pretty.is_none() {
            return false;
        }
        self.show_pretty = !self.show_pretty;
        self.scroll = 0;
        true
    }
}

fn pretty_json(raw: &str) -> Option<String> {
    let t = raw.trim();
    if !(t.starts_with('{') || t.starts_with('[')) {
        return None;
    }
    let v: serde_json::Value = serde_json::from_str(t).ok()?;
    if !(v.is_object() || v.is_array()) {
        return None;
    }
    serde_json::to_string_pretty(&v).ok()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SidebarItem {
    Database(String),
    Table(String, String),
    Column(String, String, String), // database, table, column
}

/// Per-tab editor and result state. The active tab lives directly in the
/// `App` fields; `App::tabs` holds the parked state of the others.
pub struct QueryTab {
    pub editor: Editor,
    pub editor_scroll: (usize, usize),
    pub result_views: Vec<ResultView>,
    pub result_idx: usize,
    pub table_state: TableState,
    pub first_col: usize,
    pub selected_col: usize,
    pub last_error: Option<String>,
}

impl Default for QueryTab {
    fn default() -> Self {
        Self {
            editor: Editor::new(),
            editor_scroll: (0, 0),
            result_views: Vec::new(),
            result_idx: 0,
            table_state: TableState::default(),
            first_col: 0,
            selected_col: 0,
            last_error: None,
        }
    }
}

const TAB_TITLE_WIDTH: usize = 16;

/// Short label for a tab derived from its SQL: first non-blank line,
/// whitespace collapsed, truncated.
pub fn tab_title(sql: &str) -> String {
    let line = sql.lines().find(|l| !l.trim().is_empty()).unwrap_or("");
    let collapsed: Vec<&str> = line.split_whitespace().collect();
    if collapsed.is_empty() {
        return "untitled".to_string();
    }
    let joined = collapsed.join(" ");
    if joined.chars().count() > TAB_TITLE_WIDTH {
        let cut: String = joined.chars().take(TAB_TITLE_WIDTH - 1).collect();
        format!("{cut}\u{2026}")
    } else {
        joined
    }
}

pub struct App {
    pub mode: Mode,
    pub should_quit: bool,
    pub config: Config,
    pub config_error: Option<String>,

    /// Password used to encrypt/decrypt the config file; set after successful unlock.
    pub password: Option<String>,
    /// State for the password entry screen.
    pub password_state: Option<PasswordState>,

    // connection list
    pub conn_list_selected: usize,

    // connection form
    pub form: Option<ConnForm>,

    // query screen
    pub editor: Editor,
    /// (vertical line offset, horizontal column offset)
    pub editor_scroll: (usize, usize),
    /// (width, height) of the editor viewport, updated during render
    pub editor_view: (u16, u16),
    /// Width of the results viewport, updated during render; keeps the
    /// column cursor visible when moving right.
    pub results_view_w: u16,
    pub completer: Completer,
    pub popup: Option<PopupState>,
    pub focus: Focus,
    pub sidebar_selected: usize,
    pub sidebar_scroll: usize,
    pub sidebar_expanded: HashSet<String>,
    pub sidebar_tables_expanded: HashSet<String>,
    pub table_detail_key: Option<String>,
    pub detail_scroll: usize,
    pub table_state: TableState,
    pub first_col: usize,
    /// Column cursor within the current result set; Left/Right move it.
    pub selected_col: usize,
    /// All result sets of the last successful query; one view per set.
    pub result_views: Vec<ResultView>,
    /// Index of the result set currently shown.
    pub result_idx: usize,
    /// Full text of the last failed query's error; shown in the results pane
    /// until the next query is submitted.
    pub last_error: Option<String>,
    /// Full-content popup for the selected result cell.
    pub cell_view: Option<CellView>,

    /// Parked state of every tab; the slot at `tab_idx` is a placeholder
    /// while that tab's state lives in the fields above.
    pub tabs: Vec<QueryTab>,
    pub tab_idx: usize,
    /// Tab that issued the query currently running on the worker, so its
    /// result lands there even if the user has switched tabs meanwhile.
    pub query_tab: usize,
    /// Result set within `query_tab` that a running next-batch fetch extends.
    pub page_result_idx: usize,

    pub status: (String, StatusKind),
    pub busy: Option<Busy>,
    pub conn_info: Option<ConnInfo>,
    /// Connection config used for the active session; needed to open the
    /// throwaway KILL QUERY connection when cancelling.
    pub active_conn: Option<Connection>,
    pub worker: DbWorker,
    pub export_dlg: Option<ExportDialog>,
    pub confirm: Option<Confirm>,
    pub show_help: bool,
    pub history: Vec<String>,
    /// Where history is persisted; `None` disables persistence.
    pub history_path: Option<std::path::PathBuf>,
    pub history_idx: Option<usize>,
    /// F3 history panel: open flag, substring filter, selected row.
    pub history_open: bool,
    pub history_filter: String,
    pub history_selected: usize,
}

impl App {
    pub fn new() -> Result<Self, String> {
        let (config, config_error, password_state, mode) = match Config::load() {
            Ok(crate::config::ConfigLoadResult::Encrypted { salt, nonce, ciphertext }) => (
                Config::default(),
                None,
                Some(PasswordState {
                    mode: PasswordMode::Unlock,
                    password: String::new(),
                    confirm: String::new(),
                    active: 0,
                    salt,
                    nonce,
                    ciphertext,
                    error: String::new(),
                }),
                Mode::Password,
            ),
            Ok(crate::config::ConfigLoadResult::Fresh) => (
                Config::default(),
                None,
                Some(PasswordState {
                    mode: PasswordMode::Setup,
                    password: String::new(),
                    confirm: String::new(),
                    active: 0,
                    salt: String::new(),
                    nonce: String::new(),
                    ciphertext: String::new(),
                    error: String::new(),
                }),
                Mode::Password,
            ),
            Ok(crate::config::ConfigLoadResult::PlaintextNeedsEncryption(c)) => (
                c,
                None,
                Some(PasswordState {
                    mode: PasswordMode::Setup,
                    password: String::new(),
                    confirm: String::new(),
                    active: 0,
                    salt: String::new(),
                    nonce: String::new(),
                    ciphertext: String::new(),
                    error: "Set a password to encrypt your connections".to_string(),
                }),
                Mode::Password,
            ),
            Err(e) => (Config::default(), Some(e), None, Mode::ConnList),
        };

        let history_path = crate::history::history_path();
        let (history, history_error) = match crate::history::load(&history_path) {
            Ok(h) => (h, None),
            Err(e) => (Vec::new(), Some(e)),
        };

        let mut app = Self {
            mode,
            should_quit: false,
            config,
            config_error,
            password: None,
            password_state,
            conn_list_selected: 0,
            form: None,
            editor: Editor::new(),
            editor_scroll: (0, 0),
            editor_view: (80, 10),
            results_view_w: 80,
            completer: Completer::new(),
            popup: None,
            focus: Focus::Editor,
            sidebar_selected: 0,
            sidebar_scroll: 0,
            sidebar_expanded: HashSet::new(),
            sidebar_tables_expanded: HashSet::new(),
            table_detail_key: None,
            detail_scroll: 0,
            table_state: TableState::default(),
            first_col: 0,
            selected_col: 0,
            result_views: Vec::new(),
            result_idx: 0,
            last_error: None,
            cell_view: None,
            tabs: vec![QueryTab::default()],
            tab_idx: 0,
            query_tab: 0,
            page_result_idx: 0,
            status: (String::new(), StatusKind::Info),
            busy: None,
            conn_info: None,
            active_conn: None,
            worker: DbWorker::spawn(),
            export_dlg: None,
            confirm: None,
            show_help: false,
            history,
            history_path: Some(history_path),
            history_idx: None,
            history_open: false,
            history_filter: String::new(),
            history_selected: 0,
        };
        if let Some(e) = history_error {
            app.set_status(e, StatusKind::Error);
        }
        if app.mode == Mode::ConnList {
            if let Some(i) = app.config.auto_connect
                && i < app.config.connections.len()
            {
                app.connect_index(i);
            } else if let Some(e) = &app.config_error {
                app.set_status(e.clone(), StatusKind::Error);
            }
        }
        Ok(app)
    }

    fn set_status(&mut self, msg: impl Into<String>, kind: StatusKind) {
        self.status = (msg.into(), kind);
    }

    // ---- tabs ----

    fn take_active_tab(&mut self) -> QueryTab {
        QueryTab {
            editor: std::mem::replace(&mut self.editor, Editor::new()),
            editor_scroll: std::mem::take(&mut self.editor_scroll),
            result_views: std::mem::take(&mut self.result_views),
            result_idx: std::mem::take(&mut self.result_idx),
            table_state: std::mem::take(&mut self.table_state),
            first_col: std::mem::take(&mut self.first_col),
            selected_col: std::mem::take(&mut self.selected_col),
            last_error: self.last_error.take(),
        }
    }

    fn load_tab(&mut self, tab: QueryTab) {
        self.editor = tab.editor;
        self.editor_scroll = tab.editor_scroll;
        self.result_views = tab.result_views;
        self.result_idx = tab.result_idx;
        self.table_state = tab.table_state;
        self.first_col = tab.first_col;
        self.selected_col = tab.selected_col;
        self.last_error = tab.last_error;
        self.popup = None;
        self.cell_view = None;
        self.history_idx = None;
    }

    /// Make tab `i` active, parking the current one.
    fn switch_tab(&mut self, i: usize) {
        if i == self.tab_idx || i >= self.tabs.len() {
            return;
        }
        let active = self.take_active_tab();
        self.tabs[self.tab_idx] = active;
        let next = std::mem::take(&mut self.tabs[i]);
        self.tab_idx = i;
        self.load_tab(next);
        self.set_status(
            format!("tab {}/{}", i + 1, self.tabs.len()),
            StatusKind::Info,
        );
    }

    fn cycle_tab(&mut self, dir: i32) {
        let n = self.tabs.len() as i32;
        if n < 2 {
            return;
        }
        let next = (self.tab_idx as i32 + dir).rem_euclid(n) as usize;
        self.switch_tab(next);
    }

    fn new_tab(&mut self) {
        let active = self.take_active_tab();
        self.tabs[self.tab_idx] = active;
        self.tabs.push(QueryTab::default());
        self.tab_idx = self.tabs.len() - 1;
        self.load_tab(QueryTab::default());
        self.focus = Focus::Editor;
        self.set_status(format!("new tab {}", self.tab_idx + 1), StatusKind::Info);
    }

    fn query_running_in(&self, i: usize) -> bool {
        matches!(self.busy, Some(Busy::Querying) | Some(Busy::Exporting)) && self.query_tab == i
    }

    fn close_tab(&mut self) {
        if self.tabs.len() == 1 {
            self.set_status("cannot close the last tab", StatusKind::Error);
            return;
        }
        if self.query_running_in(self.tab_idx) {
            self.set_status("a query is running in this tab", StatusKind::Error);
            return;
        }
        let closed = self.tab_idx;
        self.tabs.remove(closed);
        if self.query_tab > closed {
            self.query_tab -= 1;
        }
        let next_idx = closed.min(self.tabs.len() - 1);
        let next = std::mem::take(&mut self.tabs[next_idx]);
        self.tab_idx = next_idx;
        self.load_tab(next);
        self.focus = Focus::Editor;
        self.set_status(format!("tab {} closed", closed + 1), StatusKind::Info);
    }

    /// (title, has_running_query) for every tab, in order.
    pub fn tab_titles(&self) -> Vec<(String, bool)> {
        (0..self.tabs.len())
            .map(|i| {
                let sql = if i == self.tab_idx {
                    self.editor.text()
                } else {
                    self.tabs[i].editor.text()
                };
                (tab_title(&sql), self.query_running_in(i))
            })
            .collect()
    }

    /// The result set currently displayed.
    pub fn current_result(&self) -> Option<&ResultView> {
        self.result_views.get(self.result_idx)
    }

    /// Switch to the previous/next result set of a multi-statement query.
    fn cycle_result(&mut self, dir: i32) {
        let n = self.result_views.len();
        if n < 2 {
            return;
        }
        let cur = self.result_idx as i32;
        self.result_idx = ((cur + dir).rem_euclid(n as i32)) as usize;
        self.table_state = TableState::default();
        self.first_col = 0;
        self.selected_col = 0;
        self.set_status(
            format!("result set {}/{}", self.result_idx + 1, n),
            StatusKind::Info,
        );
    }

    /// Abort the query/export currently running on the worker thread.
    fn cancel_running(&mut self) {
        if !matches!(self.busy, Some(Busy::Querying) | Some(Busy::Exporting)) {
            return;
        }
        let Some(conn) = self.active_conn.clone() else {
            self.set_status("cannot cancel — no connection info", StatusKind::Error);
            return;
        };
        let Some(info) = self.conn_info.clone() else {
            self.set_status("cannot cancel — not connected", StatusKind::Error);
            return;
        };
        self.set_status("cancelling...", StatusKind::Info);
        self.worker.cancel(&conn, info.conn_id, info.tunnel_port);
    }

    /// Build the visible list of sidebar items (databases + expanded tables + expanded columns).
    pub fn sidebar_items(&self) -> Vec<SidebarItem> {
        let schema = self.completer.schema();
        let mut items = Vec::new();
        for db in &schema.databases {
            let db_name = db.clone();
            items.push(SidebarItem::Database(db_name.clone()));
            if self.sidebar_expanded.contains(&db_name) {
                for table in schema.tables_by_db.get(&db_name).map(|v| v.as_slice()).unwrap_or(&[]) {
                    items.push(SidebarItem::Table(db_name.clone(), table.clone()));
                    let table_key = format!("{}.{}", db_name, table);
                    if self.sidebar_tables_expanded.contains(&table_key)
                        && let Some(cols) = schema.column_names.get(&table_key)
                    {
                        for col in cols {
                            items.push(SidebarItem::Column(
                                db_name.clone(),
                                table.clone(),
                                col.clone(),
                            ));
                        }
                    }
                }
            }
        }
        items
    }

    /// Request table details if not already cached.
    fn maybe_fetch_table_detail(&mut self, db: String, table: String) {
        let table_key = format!("{}.{}", db, table);
        if !self.completer.schema().column_names.contains_key(&table_key) {
            self.worker.send(DbCommand::FetchTableDetail(db, table));
        }
    }

    fn open_table_detail_from_sidebar(&mut self) {
        let items = self.sidebar_items();
        let (db, table) = match items.get(self.sidebar_selected) {
            Some(SidebarItem::Table(db, table)) => (db.clone(), table.clone()),
            Some(SidebarItem::Column(db, table, _)) => (db.clone(), table.clone()),
            _ => {
                self.set_status("select a table first, then press F2", StatusKind::Error);
                return;
            }
        };
        let table_key = format!("{}.{}", db, table);
        self.table_detail_key = Some(table_key.clone());
        self.detail_scroll = 0;
        self.maybe_fetch_table_detail(db, table);
    }

    fn copy_to_clipboard(&mut self, text: String, what: &str) {
        let preview: String = text.chars().take(60).collect();
        let suffix = if text.chars().count() > 60 { "..." } else { "" };
        match arboard::Clipboard::new() {
            Ok(mut clipboard) => match clipboard.set_text(text) {
                Ok(()) => self.set_status(
                    format!("{what} copied: {preview}{suffix}"),
                    StatusKind::Ok,
                ),
                Err(e) => self.set_status(format!("clipboard error: {e}"), StatusKind::Error),
            },
            Err(e) => self.set_status(format!("clipboard not available: {e}"), StatusKind::Error),
        }
    }

    /// Text of the cell at (selected row, selected column).
    fn cell_text(&self) -> Option<String> {
        let rv = self.current_result()?;
        let row = self.table_state.selected().unwrap_or(0);
        rv.display
            .get(row)
            .and_then(|r| r.get(self.selected_col))
            .cloned()
    }

    /// Selected row as tab-separated values.
    fn row_text(&self) -> Option<String> {
        let rv = self.current_result()?;
        let row = self.table_state.selected().unwrap_or(0);
        rv.display.get(row).map(|cells| cells.join("\t"))
    }

    /// Copy the cell at (selected row, selected col) of the current result set.
    fn copy_cell(&mut self) {
        match self.cell_text() {
            Some(t) => self.copy_to_clipboard(t, "cell"),
            None => self.set_status("no result to copy", StatusKind::Error),
        }
    }

    /// Copy the selected row of the current result set as tab-separated values.
    fn copy_row(&mut self) {
        match self.row_text() {
            Some(t) => self.copy_to_clipboard(t, "row"),
            None => self.set_status("no result to copy", StatusKind::Error),
        }
    }

    fn copy_table_detail(&mut self) {
        let Some(key) = self.table_detail_key.clone() else {
            return;
        };
        let schema = self.completer.schema();
        let text = match schema.ddl.get(&key) {
            Some(ddl) if !ddl.is_empty() => ddl.clone(),
            _ => {
                self.set_status("DDL not available yet", StatusKind::Error);
                return;
            }
        };
        self.copy_to_clipboard(text, "DDL");
    }

    pub fn query_dialog_open(&self) -> bool {
        self.export_dlg.is_some()
            || self.confirm.is_some()
            || self.show_help
            || self.table_detail_key.is_some()
            || self.history_open
            || self.cell_view.is_some()
    }

    /// Open the full-content popup for the cell at (selected row, selected col).
    fn open_cell_view(&mut self) {
        let Some(rv) = self.current_result() else {
            self.set_status("no result to view", StatusKind::Error);
            return;
        };
        if rv.result.rows.is_empty() {
            self.set_status("empty result set", StatusKind::Error);
            return;
        }
        let row = self.table_state.selected().unwrap_or(0).min(rv.result.rows.len() - 1);
        let col = self.selected_col.min(rv.result.columns.len().saturating_sub(1));
        let column = rv.result.columns[col].clone();
        let raw = rv.display[row].get(col).cloned().unwrap_or_default();
        self.table_state.select(Some(row));
        self.selected_col = col;
        self.cell_view = Some(CellView::new(column, row, raw));
    }

    /// Move the cell cursor while the viewer is open and reload it.
    fn cell_view_move(&mut self, drow: i32, dcol: i32) {
        let Some(rv) = self.current_result() else {
            return;
        };
        let rows = rv.result.rows.len() as i32;
        let cols = rv.result.columns.len() as i32;
        let row = (self.table_state.selected().unwrap_or(0) as i32 + drow).clamp(0, rows - 1);
        let col = (self.selected_col as i32 + dcol).clamp(0, cols - 1);
        if row as usize == self.table_state.selected().unwrap_or(0) && col as usize == self.selected_col {
            return;
        }
        self.table_state.select(Some(row as usize));
        self.selected_col = col as usize;
        self.ensure_col_visible();
        self.open_cell_view();
    }

    fn on_key_cell_view(&mut self, key: KeyEvent) {
        let Some(view) = self.cell_view.as_mut() else {
            return;
        };
        let page = view.page.max(1);
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') | KeyCode::Enter => {
                self.cell_view = None;
            }
            KeyCode::Up | KeyCode::Char('k') => view.scroll = view.scroll.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j') => view.scroll = view.scroll.saturating_add(1),
            KeyCode::PageUp | KeyCode::Char('u') => view.scroll = view.scroll.saturating_sub(page),
            KeyCode::PageDown | KeyCode::Char('d') | KeyCode::Char(' ') => {
                view.scroll = view.scroll.saturating_add(page)
            }
            KeyCode::Home | KeyCode::Char('g') => view.scroll = 0,
            // the renderer clamps this to the last page
            KeyCode::End | KeyCode::Char('G') => view.scroll = usize::MAX,
            KeyCode::Char('p') => {
                if !view.toggle_pretty() {
                    self.set_status("cell is not a JSON object/array", StatusKind::Info);
                }
            }
            KeyCode::Char('y') => {
                let text = view.text().to_string();
                self.copy_to_clipboard(text, "cell");
            }
            KeyCode::Left | KeyCode::Char('h') => self.cell_view_move(0, -1),
            KeyCode::Right | KeyCode::Char('l') => self.cell_view_move(0, 1),
            KeyCode::Char('K') => self.cell_view_move(-1, 0),
            KeyCode::Char('J') => self.cell_view_move(1, 0),
            _ => {}
        }
    }

    /// True when the focused SQL editor has a non-empty selection.
    fn editor_selection_active(&self) -> bool {
        self.mode == Mode::Query
            && self.focus == Focus::Editor
            && !self.query_dialog_open()
            && self.editor.has_selection()
    }

    fn copy_selection(&mut self) {
        if let Some(text) = self.editor.selected_text() {
            self.copy_to_clipboard(text, "selection");
        }
    }

    fn cut_selection(&mut self) {
        let Some(text) = self.editor.selected_text() else {
            self.set_status("nothing selected", StatusKind::Error);
            return;
        };
        match arboard::Clipboard::new().and_then(|mut c| c.set_text(text)) {
            Ok(()) => {
                self.editor.delete_selection();
                self.clamp_editor_scroll();
                self.update_popup();
                self.set_status("selection cut", StatusKind::Ok);
            }
            Err(e) => self.set_status(format!("clipboard error: {e}"), StatusKind::Error),
        }
    }

    fn paste_from_clipboard(&mut self) {
        match arboard::Clipboard::new().and_then(|mut c| c.get_text()) {
            Ok(text) => self.on_paste(text),
            Err(e) => self.set_status(format!("clipboard error: {e}"), StatusKind::Error),
        }
    }

    fn redo(&mut self) {
        if self.editor.redo() {
            self.clamp_editor_scroll();
            self.popup = None;
        } else {
            self.set_status("nothing to redo", StatusKind::Info);
        }
    }

    /// Terminal setup, main event loop, teardown.
    pub fn run(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        let mut stdout = io::stdout();
        enable_raw_mode()?;
        execute!(stdout, EnterAlternateScreen, EnableBracketedPaste)?;
        let backend = CrosstermBackend::new(stdout);
        let mut terminal = Terminal::new(backend)?;

        let result = self.event_loop(&mut terminal);

        disable_raw_mode()?;
        execute!(
            terminal.backend_mut(),
            LeaveAlternateScreen,
            DisableBracketedPaste
        )?;
        terminal.show_cursor()?;
        result
    }

    fn event_loop(
        &mut self,
        terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    ) -> Result<(), Box<dyn std::error::Error>> {
        loop {
            while let Some(ev) = self.worker.poll_event() {
                self.on_db_event(ev);
            }
            terminal.draw(|f| ui::draw(f, self))?;
            if self.should_quit {
                return Ok(());
            }
            // bounded wait keeps async db events flowing into the UI
            if event::poll(Duration::from_millis(120))? {
                match event::read()? {
                    Event::Key(key) if key.kind == KeyEventKind::Press => self.on_key(key),
                    Event::Paste(text) => self.on_paste(text),
                    _ => {}
                }
            }
        }
    }

    fn save_config(&mut self) {
        if let Err(e) = self.config.save(self.password.as_deref()) {
            self.set_status(e, StatusKind::Error);
        }
    }

    // ---- connection handling ----

    fn connect_index(&mut self, i: usize) {
        let conn = self.config.connections[i].clone();
        self.active_conn = Some(conn.clone());
        self.mode = Mode::Query;
        self.busy = Some(Busy::Connecting);
        self.popup = None;
        self.focus = Focus::Editor;
        self.set_status(
            format!("connecting to {}...", conn.display_name()),
            StatusKind::Info,
        );
        self.worker.send(DbCommand::Connect(conn));
    }

    fn delete_connection(&mut self, i: usize) {
        self.config.connections.remove(i);
        if let Some(sel) = self.config.selected.as_mut()
            && *sel >= i
            && *sel > 0
        {
            *sel -= 1;
        }
        if let Some(ac) = self.config.auto_connect {
            if ac == i {
                self.config.auto_connect = None;
            } else if ac > i {
                self.config.auto_connect = Some(ac - 1);
            }
        }
        if self.conn_list_selected >= self.config.connections.len() {
            self.conn_list_selected = self.config.connections.len().saturating_sub(1);
        }
        self.save_config();
        self.set_status("connection deleted", StatusKind::Ok);
    }

    // ---- query execution ----

    fn submit_query(&mut self) {
        let text = self.editor.text().trim().to_string();
        if text.is_empty() {
            self.set_status("nothing to run", StatusKind::Error);
            return;
        }
        if self.busy.is_some() {
            self.set_status("busy — wait for the current operation", StatusKind::Error);
            return;
        }
        if self.history.last().map(|h| h.as_str()) != Some(text.as_str()) {
            self.history.push(text.clone());
            if self.history.len() > crate::history::MAX_HISTORY {
                self.history.remove(0);
            }
            self.save_history();
        }
        self.history_idx = None;

        if is_destructive(&text) {
            let preview: String = text.chars().take(80).collect();
            let suffix = if text.len() > 80 { "…" } else { "" };
            self.confirm = Some(Confirm {
                message: format!("Execute: {}{}", preview, suffix),
                action: ConfirmAction::RunDestructive(text),
            });
            return;
        }

        let sql = apply_default_limit(&text, self.config.default_limit);
        self.execute_query(sql);
    }

    fn execute_query(&mut self, sql: String) {
        self.busy = Some(Busy::Querying);
        self.query_tab = self.tab_idx;
        self.popup = None;
        self.last_error = None;
        self.set_status("running query...", StatusKind::Info);
        self.worker.send(DbCommand::Query(sql));
    }

    /// Fetch the batch after the rows already shown for the current result set.
    fn load_next_page(&mut self) {
        if self.busy.is_some() {
            self.set_status("busy — wait for the current operation", StatusKind::Error);
            return;
        }
        let Some(rv) = self.current_result() else {
            return;
        };
        if !rv.result.has_more {
            self.set_status("no more rows to load", StatusKind::Info);
            return;
        }
        let Some((sql, page)) = rv.result.next_page() else {
            self.set_status("this result set cannot be paged", StatusKind::Error);
            return;
        };
        self.busy = Some(Busy::Querying);
        self.query_tab = self.tab_idx;
        self.page_result_idx = self.result_idx;
        self.set_status(format!("loading next {page} rows..."), StatusKind::Info);
        self.worker.send(DbCommand::QueryPage { sql, page });
    }

    fn save_history(&mut self) {
        let Some(path) = self.history_path.clone() else {
            return;
        };
        if let Err(e) = crate::history::save(&path, &self.history) {
            self.set_status(e, StatusKind::Error);
        }
    }

    fn history_move(&mut self, older: bool) {
        if self.history.is_empty() {
            return;
        }
        let idx = match self.history_idx {
            None => {
                if older {
                    self.history.len() - 1
                } else {
                    return;
                }
            }
            Some(i) => {
                if older {
                    i.saturating_sub(1)
                } else {
                    (i + 1).min(self.history.len() - 1)
                }
            }
        };
        self.history_idx = Some(idx);
        self.editor = Editor::from_text(&self.history[idx]);
        self.editor_scroll = (0, 0);
        self.popup = None;
        self.set_status(
            format!("history {}/{}", idx + 1, self.history.len()),
            StatusKind::Info,
        );
    }

    // ---- history panel (F3) ----

    /// Indices into `history` matching the current filter, newest first.
    pub fn filtered_history(&self) -> Vec<usize> {
        let needle = self.history_filter.to_lowercase();
        self.history
            .iter()
            .enumerate()
            .filter(|(_, h)| needle.is_empty() || h.to_lowercase().contains(&needle))
            .map(|(i, _)| i)
            .rev()
            .collect()
    }

    fn open_history(&mut self) {
        self.history_open = true;
        self.history_filter.clear();
        self.history_selected = 0;
    }

    fn on_key_history(&mut self, key: KeyEvent) {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Esc | KeyCode::Tab => {
                self.history_open = false;
            }
            KeyCode::Up => {
                self.history_selected = self.history_selected.saturating_sub(1);
            }
            KeyCode::Down => {
                let n = self.filtered_history().len();
                if self.history_selected + 1 < n {
                    self.history_selected += 1;
                }
            }
            KeyCode::Backspace => {
                self.history_filter.pop();
                self.history_selected = 0;
            }
            KeyCode::Char('d') if ctrl => {
                let idxs = self.filtered_history();
                if let Some(&i) = idxs.get(self.history_selected) {
                    self.history.remove(i);
                    self.history_idx = None;
                    self.save_history();
                    let n = self.filtered_history().len();
                    self.history_selected = self.history_selected.min(n.saturating_sub(1));
                    self.set_status("history entry deleted", StatusKind::Info);
                }
            }
            KeyCode::Enter => {
                let idxs = self.filtered_history();
                if let Some(&i) = idxs.get(self.history_selected) {
                    let text = self.history[i].clone();
                    self.editor = Editor::from_text(&text);
                    self.editor_scroll = (0, 0);
                    self.popup = None;
                    self.focus = Focus::Editor;
                    self.history_open = false;
                    self.set_status("loaded from history", StatusKind::Ok);
                }
            }
            KeyCode::Char(c) if !ctrl => {
                self.history_filter.push(c);
                self.history_selected = 0;
            }
            _ => {}
        }
    }

    // ---- completion ----

    fn update_popup(&mut self) {
        let (_, word) = self.editor.word_before_cursor();
        let has_word = !word.trim_matches('`').is_empty();
        if !has_word {
            self.popup = None;
            return;
        }
        let suggestions = self.completer.compute(&self.editor);
        if suggestions.is_empty() {
            self.popup = None;
        } else {
            self.popup = Some(PopupState {
                suggestions,
                selected: 0,
            });
        }
    }

    fn accept_suggestion(&mut self) {
        let sugg = self.popup.as_ref().map(|p| {
            p.suggestions
                .get(p.selected)
                .cloned()
                .unwrap_or_else(|| p.suggestions[0].clone())
        });
        if let Some(s) = sugg {
            crate::completer::Completer::apply(&mut self.editor, &s);
            self.update_popup();
        }
    }

    fn popup_navigate(&mut self, up: bool) -> bool {
        if let Some(p) = self.popup.as_mut() {
            let n = p.suggestions.len();
            if up {
                p.selected = (p.selected + n - 1) % n;
            } else {
                p.selected = (p.selected + 1) % n;
            }
            true
        } else {
            false
        }
    }

    // ---- editor scroll clamping ----

    pub(crate) fn clamp_editor_scroll(&mut self) {
        let (w, h) = self.editor_view;
        let (line, off) = self.editor.cursor;
        let h = h.max(1) as usize;
        let w = w.max(1) as usize;
        let col: usize = self.editor.lines()[line][..off].chars().count();
        if line < self.editor_scroll.0 {
            self.editor_scroll.0 = line;
        }
        if h <= line - self.editor_scroll.0 + 1 {
            self.editor_scroll.0 = line + 1 - h;
        }
        if col < self.editor_scroll.1 {
            self.editor_scroll.1 = col;
        }
        if w <= col - self.editor_scroll.1 + 1 {
            self.editor_scroll.1 = col + 1 - w;
        }
    }

    // ---- db events ----

    pub fn on_db_event(&mut self, ev: DbEvent) {
        match ev {
            DbEvent::Connected(info) => {
                self.conn_info = Some(info.clone());
                self.busy = None;
                let mut msg = format!(
                    "connected — MySQL {} ({})",
                    info.server_version,
                    info.label()
                );
                if let Some(cipher) = &info.ssl_cipher {
                    msg.push_str(&format!(" TLS {cipher}"));
                }
                self.set_status(msg, StatusKind::Ok);
            }
            DbEvent::ConnectFailed(e) => {
                self.busy = None;
                self.conn_info = None;
                self.active_conn = None;
                self.mode = Mode::ConnList;
                self.history_open = false;
                self.set_status(format!("connect failed: {e}"), StatusKind::Error);
            }
            DbEvent::QueryDone { results, message } => {
                self.busy = None;
                let elapsed = results.first().map(|r| r.elapsed.as_secs_f64());
                if !results.is_empty() {
                    let views: Vec<ResultView> = results.into_iter().map(ResultView::new).collect();
                    if self.query_tab == self.tab_idx {
                        self.result_views = views;
                        self.result_idx = 0;
                        self.table_state = TableState::default();
                        self.first_col = 0;
                        self.selected_col = 0;
                        self.cell_view = None;
                    } else if let Some(tab) = self.tabs.get_mut(self.query_tab) {
                        tab.result_views = views;
                        tab.result_idx = 0;
                        tab.table_state = TableState::default();
                        tab.first_col = 0;
                        tab.selected_col = 0;
                    }
                }
                let mut msg = message;
                if let Some(secs) = elapsed {
                    msg = format!("{msg} in {secs:.3}s");
                }
                if self.query_tab != self.tab_idx {
                    msg = format!("[tab {}] {msg}", self.query_tab + 1);
                }
                self.set_status(msg, StatusKind::Ok);
            }
            DbEvent::QueryFailed(e) => {
                self.busy = None;
                if e == "query cancelled" {
                    self.set_status(e, StatusKind::Info);
                } else {
                    let mut first_line = e.lines().next().unwrap_or("").to_string();
                    if self.query_tab == self.tab_idx {
                        self.last_error = Some(e);
                    } else if let Some(tab) = self.tabs.get_mut(self.query_tab) {
                        tab.last_error = Some(e);
                        first_line = format!("[tab {}] {first_line}", self.query_tab + 1);
                    }
                    self.set_status(first_line, StatusKind::Error);
                }
            }
            DbEvent::PageDone {
                rows,
                has_more,
                elapsed,
            } => {
                self.busy = None;
                let n = rows.len();
                let (tab, idx) = (self.query_tab, self.page_result_idx);
                let views = if tab == self.tab_idx {
                    Some(&mut self.result_views)
                } else {
                    self.tabs.get_mut(tab).map(|t| &mut t.result_views)
                };
                let Some(rv) = views.and_then(|v| v.get_mut(idx)) else {
                    self.set_status("result set is gone; batch discarded", StatusKind::Info);
                    return;
                };
                rv.append(rows, has_more);
                let total = rv.result.rows.len();
                let mut msg = if n == 0 {
                    format!("no more rows ({total} total)")
                } else {
                    format!(
                        "loaded {n} more row{} ({total} total) in {:.3}s",
                        if n == 1 { "" } else { "s" },
                        elapsed.as_secs_f64()
                    )
                };
                if tab != self.tab_idx {
                    msg = format!("[tab {}] {msg}", tab + 1);
                }
                self.set_status(msg, StatusKind::Ok);
            }
            DbEvent::PageFailed(e) => {
                self.busy = None;
                if e == "query cancelled" {
                    self.set_status(e, StatusKind::Info);
                } else {
                    let first_line = e.lines().next().unwrap_or("").to_string();
                    self.set_status(format!("load more failed: {first_line}"), StatusKind::Error);
                }
            }
            DbEvent::CancelFailed(e) => {
                if self.busy.is_some() {
                    self.set_status(e, StatusKind::Error);
                }
            }
            DbEvent::SchemaUpdated(schema) => {
                if matches!(self.busy, Some(Busy::Querying)) {
                    self.busy = None;
                }
                let tables = schema.table_count();
                let cur_db = schema.database.clone();
                self.completer.update_schema(schema);
                if !cur_db.is_empty() {
                    self.sidebar_expanded.insert(cur_db.clone());
                }
                let items = self.sidebar_items();
                if items.is_empty() {
                    self.sidebar_selected = 0;
                } else if let Some(idx) = items.iter().position(|i| {
                    matches!(i, SidebarItem::Database(d) if d == &cur_db)
                }) {
                    self.sidebar_selected = idx;
                } else {
                    self.sidebar_selected = self.sidebar_selected.min(items.len() - 1);
                }
                if self.busy.is_none() {
                    self.set_status(format!("schema loaded — {tables} tables"), StatusKind::Ok);
                }
                self.update_popup();
            }
            DbEvent::TableDetailFetched {
                database,
                table,
                columns,
                ddl,
            } => {
                let table_key = format!("{}.{}", database, table);
                let schema = self.completer.schema_mut();
                schema.column_names.insert(table_key.clone(), columns);
                schema.ddl.insert(table_key, ddl);
            }
            DbEvent::Disconnected => {
                self.conn_info = None;
                self.active_conn = None;
                self.busy = None;
                self.mode = Mode::ConnList;
                self.history_open = false;
                self.last_error = None;
                self.cell_view = None;
                self.set_status("disconnected", StatusKind::Info);
            }
            DbEvent::ExportProgress(rows) => {
                self.set_status(format!("exporting... {rows} rows written"), StatusKind::Info);
            }
            DbEvent::ExportDone { result } => {
                self.busy = None;
                self.export_dlg = None;
                match result {
                    Ok((path, n)) => {
                        self.set_status(
                            format!("exported {n} rows to {}", path.display()),
                            StatusKind::Ok,
                        );
                    }
                    Err(e) => {
                        self.set_status(format!("export failed: {e}"), StatusKind::Error);
                    }
                }
            }
        }
    }

    // ---- key handling ----

    pub fn on_key(&mut self, key: KeyEvent) {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        if ctrl && key.code == KeyCode::Char('c') && self.editor_selection_active() {
            self.copy_selection();
            return;
        }
        if ctrl && matches!(key.code, KeyCode::Char('q' | 'c')) {
            self.should_quit = true;
            return;
        }
        match self.mode {
            Mode::Password => self.on_key_password(key),
            Mode::ConnList => self.on_key_conn_list(key),
            Mode::ConnForm => self.on_key_conn_form(key),
            Mode::Query => self.on_key_query(key),
        }
    }

    fn on_key_password(&mut self, key: KeyEvent) {
        let Some(mut ps) = self.password_state.take() else {
            return;
        };
        match key.code {
            KeyCode::Esc => {
                self.should_quit = true;
                self.password_state = Some(ps);
                return;
            }
            KeyCode::Tab | KeyCode::Down => {
                if ps.mode == PasswordMode::Setup {
                    ps.active = (ps.active + 1) % 2;
                }
            }
            KeyCode::BackTab | KeyCode::Up => {
                if ps.mode == PasswordMode::Setup {
                    ps.active = if ps.active == 0 { 1 } else { 0 };
                }
            }
            KeyCode::Enter => match ps.mode {
                PasswordMode::Setup => {
                    if ps.password.is_empty() {
                        ps.error = "password cannot be empty".to_string();
                        self.password_state = Some(ps);
                        return;
                    }
                    if ps.password != ps.confirm {
                        ps.error = "passwords do not match".to_string();
                        ps.confirm.clear();
                        ps.active = 1;
                        self.password_state = Some(ps);
                        return;
                    }
                    if let Err(e) = self.config.save(Some(&ps.password)) {
                        ps.error = e;
                        self.password_state = Some(ps);
                        return;
                    }
                    self.password = Some(ps.password);
                    self.password_state = None;
                    self.mode = Mode::ConnList;
                    if let Some(i) = self.config.auto_connect
                        && i < self.config.connections.len()
                    {
                        self.connect_index(i);
                    }
                    return;
                }
                PasswordMode::Unlock => {
                    if ps.password.is_empty() {
                        ps.error = "enter your password".to_string();
                        self.password_state = Some(ps);
                        return;
                    }
                    match crate::crypto::decrypt(&ps.password, &ps.salt, &ps.nonce, &ps.ciphertext) {
                        Ok(bytes) => match Config::from_toml_bytes(&bytes) {
                            Ok(cfg) => {
                                self.config = cfg;
                                self.password = Some(ps.password);
                                self.password_state = None;
                                self.mode = Mode::ConnList;
                                self.config_error = None;
                                if let Some(i) = self.config.auto_connect
                                    && i < self.config.connections.len()
                                {
                                    self.connect_index(i);
                                }
                                return;
                            }
                            Err(e) => {
                                ps.error = e;
                                ps.password.clear();
                            }
                        },
                        Err(e) => {
                            ps.error = e;
                            ps.password.clear();
                        }
                    }
                }
            },
            KeyCode::Backspace => {
                let field = if ps.active == 0 {
                    &mut ps.password
                } else {
                    &mut ps.confirm
                };
                field.pop();
            }
            KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                let field = if ps.active == 0 {
                    &mut ps.password
                } else {
                    &mut ps.confirm
                };
                field.push(c);
            }
            _ => {}
        }
        self.password_state = Some(ps);
    }

    fn on_key_conn_list(&mut self, key: KeyEvent) {
        let n = self.config.connections.len();
        match key.code {
            KeyCode::Char('q') => self.should_quit = true,
            KeyCode::Char('k') | KeyCode::Up if n > 0 => {
                self.conn_list_selected = (self.conn_list_selected + n - 1) % n;
            }
            KeyCode::Char('j') | KeyCode::Down if n > 0 => {
                self.conn_list_selected = (self.conn_list_selected + 1) % n;
            }
            KeyCode::Enter if n > 0 => {
                self.config.selected = Some(self.conn_list_selected);
                self.connect_index(self.conn_list_selected);
            }
            KeyCode::Char('n') => {
                self.form = Some(ConnForm::from_connection(None, &Connection::default()));
                self.mode = Mode::ConnForm;
            }
            KeyCode::Char('e') if n > 0 => {
                let conn = self.config.connections[self.conn_list_selected].clone();
                self.form = Some(ConnForm::from_connection(
                    Some(self.conn_list_selected),
                    &conn,
                ));
                self.mode = Mode::ConnForm;
            }
            KeyCode::Char('d') if n > 0 => {
                let name = self.config.connections[self.conn_list_selected]
                    .display_name()
                    .to_string();
                self.confirm = Some(Confirm {
                    message: format!("Delete connection \"{name}\"?"),
                    action: ConfirmAction::DeleteConnection(self.conn_list_selected),
                });
            }
            KeyCode::Char('a') if n > 0 => {
                let i = self.conn_list_selected;
                self.config.auto_connect = if self.config.auto_connect == Some(i) {
                    None
                } else {
                    Some(i)
                };
                self.save_config();
                let msg = match self.config.auto_connect {
                    Some(_) => "auto-connect enabled",
                    None => "auto-connect disabled",
                };
                self.set_status(msg, StatusKind::Ok);
            }
            _ => {}
        }
    }

    fn on_key_conn_form(&mut self, key: KeyEvent) {
        if self.form.is_none() {
            return;
        }
        // self-level actions first so field borrows below stay scoped
        match key.code {
            KeyCode::Esc => {
                self.form = None;
                self.mode = Mode::ConnList;
                return;
            }
            KeyCode::Char('s') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.save_form();
                return;
            }
            KeyCode::Enter
                if self
                    .form
                    .as_ref()
                    .is_some_and(|f| f.active == f.fields.len() - 1) =>
            {
                self.save_form();
                return;
            }
            _ => {}
        }
        let Some(form) = self.form.as_mut() else {
            return;
        };
        let field_count = form.fields.len();
        match key.code {
            KeyCode::Tab => {
                form.active = (form.active + 1) % field_count;
            }
            KeyCode::BackTab => {
                form.active = (form.active + field_count - 1) % field_count;
            }
            KeyCode::Up => {
                form.active = (form.active + field_count - 1) % field_count;
            }
            KeyCode::Down | KeyCode::Enter => {
                form.active = (form.active + 1) % field_count;
            }
            KeyCode::Left => form.fields[form.active].left(),
            KeyCode::Right => form.fields[form.active].right(),
            KeyCode::Home => form.fields[form.active].cursor = 0,
            KeyCode::End => form.fields[form.active].cursor = form.fields[form.active].value.len(),
            KeyCode::Backspace => form.fields[form.active].backspace(),
            KeyCode::Delete => form.fields[form.active].delete(),
            KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                form.fields[form.active].insert(c);
            }
            _ => {}
        }
    }

    fn save_form(&mut self) {
        let Some(form) = self.form.take() else { return };
        let conn = match form.to_connection() {
            Ok(c) => c,
            Err(e) => {
                self.set_status(e, StatusKind::Error);
                self.form = Some(form);
                return;
            }
        };
        match form.index {
            Some(i) => {
                self.config.connections[i] = conn;
                self.set_status("connection saved", StatusKind::Ok);
                self.save_config();
                self.mode = Mode::ConnList;
            }
            None => {
                self.config.connections.push(conn);
                self.conn_list_selected = self.config.connections.len() - 1;
                self.set_status("connection saved", StatusKind::Ok);
                self.save_config();
                self.mode = Mode::ConnList;
            }
        }
    }

    fn on_key_query(&mut self, key: KeyEvent) {
        // dialogs first
        if self.show_help {
            match key.code {
                KeyCode::Esc | KeyCode::Char('?') | KeyCode::F(1) => self.show_help = false,
                _ => {}
            }
            return;
        }
        if self.confirm.is_some() {
            match key.code {
                KeyCode::Enter | KeyCode::Char('y') | KeyCode::Char('Y') => {
                    let confirm = self.confirm.take().unwrap();
                    match confirm.action {
                        ConfirmAction::DeleteConnection(i) => {
                            self.delete_connection(i);
                            self.mode = Mode::ConnList;
                        }
                        ConfirmAction::RunDestructive(sql) => {
                            self.execute_query(sql);
                        }
                    }
                }
                KeyCode::Esc | KeyCode::Char('n') | KeyCode::Char('N') => {
                    self.confirm = None;
                    self.set_status("cancelled", StatusKind::Info);
                }
                _ => {}
            }
            return;
        }
        if self.export_dlg.is_some() {
            let key_code = key.code;

            if key_code == KeyCode::Esc {
                self.export_dlg = None;
                return;
            }

            if key_code == KeyCode::Enter {
                let Some(dlg) = self.export_dlg.take() else { return };
                let path = dlg.path.clone();
                let format = dlg.format;
                let truncate = dlg.truncate;
                if truncate {
                    self.do_export_all(dlg);
                } else {
                    self.export_dlg = Some(dlg);
                    self.do_export(&path, format);
                }
                return;
            }

            let Some(dlg) = self.export_dlg.as_mut() else {
                return;
            };
            match key.code {
                KeyCode::Tab | KeyCode::BackTab => {
                    dlg.active = 1 - dlg.active;
                }
                KeyCode::Left if dlg.active == 1 => {
                    let all = ExportFormat::ALL;
                    let cur = all.iter().position(|f| *f == dlg.format).unwrap_or(0);
                    dlg.format = all[(cur + all.len() - 1) % all.len()];
                }
                KeyCode::Right if dlg.active == 1 => {
                    let all = ExportFormat::ALL;
                    let cur = all.iter().position(|f| *f == dlg.format).unwrap_or(0);
                    dlg.format = all[(cur + 1) % all.len()];
                }
                KeyCode::Left if dlg.active == 0 => {
                    if dlg.cursor > 0 {
                        dlg.cursor -= 1;
                        while dlg.cursor > 0 && !dlg.path.is_char_boundary(dlg.cursor) {
                            dlg.cursor -= 1;
                        }
                    }
                }
                KeyCode::Right if dlg.active == 0 => {
                    if dlg.cursor < dlg.path.len() {
                        dlg.cursor += 1;
                        while dlg.cursor < dlg.path.len() && !dlg.path.is_char_boundary(dlg.cursor)
                        {
                            dlg.cursor += 1;
                        }
                    }
                }
                KeyCode::Home if dlg.active == 0 => dlg.cursor = 0,
                KeyCode::End if dlg.active == 0 => dlg.cursor = dlg.path.len(),
                KeyCode::Backspace if dlg.active == 0 => {
                    if dlg.cursor > 0 {
                        let mut cut = dlg.cursor - 1;
                        while cut > 0 && !dlg.path.is_char_boundary(cut) {
                            cut -= 1;
                        }
                        dlg.path.replace_range(cut..dlg.cursor, "");
                        dlg.cursor = cut;
                    }
                }
                KeyCode::Delete if dlg.active == 0 => {
                    if dlg.cursor < dlg.path.len() {
                        let mut end = dlg.cursor + 1;
                        while end < dlg.path.len() && !dlg.path.is_char_boundary(end) {
                            end += 1;
                        }
                        dlg.path.replace_range(dlg.cursor..end, "");
                    }
                }
                KeyCode::Char(c)
                    if !key.modifiers.contains(KeyModifiers::CONTROL) && dlg.active == 0 =>
                {
                    dlg.path.insert(dlg.cursor, c);
                    dlg.cursor += c.len_utf8();
                }
                _ => {}
            }
            return;
        }

        if self.cell_view.is_some() {
            self.on_key_cell_view(key);
            return;
        }

        if self.table_detail_key.is_some() {
            match key.code {
                KeyCode::Esc | KeyCode::Char('q') => {
                    self.table_detail_key = None;
                    self.detail_scroll = 0;
                }
                KeyCode::Up | KeyCode::Char('k') => {
                    self.detail_scroll = self.detail_scroll.saturating_sub(1);
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    self.detail_scroll = self.detail_scroll.saturating_add(1);
                }
                KeyCode::Char('y') => {
                    self.copy_table_detail();
                }
                _ => {}
            }
            return;
        }

        if self.history_open {
            self.on_key_history(key);
            return;
        }

        // dialogs are gone: Esc aborts a running query/export
        // (an open completion popup still consumes the first Esc)
        if key.code == KeyCode::Esc
            && matches!(self.busy, Some(Busy::Querying) | Some(Busy::Exporting))
            && self.popup.is_none()
        {
            self.cancel_running();
            return;
        }

        // global query-mode keys
        match (key.modifiers, key.code) {
            (m, KeyCode::Char('d')) if m.contains(KeyModifiers::CONTROL) => {
                self.worker.send(DbCommand::Disconnect);
                return;
            }
            (m, KeyCode::Char('e')) if m.contains(KeyModifiers::CONTROL) => {
                if let Some(rv) = self.current_result() {
                    let truncated = rv.result.truncated;
                    let path = format!("~/export.{}", ExportFormat::Csv.default_ext());
                    let cursor = path.len();
                    self.export_dlg = Some(ExportDialog {
                        path,
                        cursor,
                        format: ExportFormat::Csv,
                        active: 0,
                        truncate: truncated,
                    });
                } else {
                    self.set_status("no result to export", StatusKind::Error);
                }
                return;
            }
            (m, KeyCode::Char('f')) if m.contains(KeyModifiers::CONTROL) => {
                self.busy = Some(Busy::Querying);
                self.last_error = None;
                self.set_status("refreshing schema...", StatusKind::Info);
                self.worker.send(DbCommand::RefreshSchema);
                return;
            }
            (m, KeyCode::Char('t')) if m.contains(KeyModifiers::CONTROL) => {
                self.new_tab();
                return;
            }
            (m, KeyCode::Char('w')) if m.contains(KeyModifiers::CONTROL) => {
                self.close_tab();
                return;
            }
            (m, KeyCode::Left) if m.contains(KeyModifiers::ALT) => {
                self.cycle_tab(-1);
                return;
            }
            (m, KeyCode::Right) if m.contains(KeyModifiers::ALT) => {
                self.cycle_tab(1);
                return;
            }
            (m, KeyCode::PageUp) if m.contains(KeyModifiers::CONTROL) => {
                self.cycle_tab(-1);
                return;
            }
            (m, KeyCode::PageDown) if m.contains(KeyModifiers::CONTROL) => {
                self.cycle_tab(1);
                return;
            }
            (m, KeyCode::Char(c @ '1'..='9')) if m.contains(KeyModifiers::ALT) => {
                let i = c as usize - '1' as usize;
                if i < self.tabs.len() {
                    self.switch_tab(i);
                } else {
                    self.set_status(format!("no tab {c}"), StatusKind::Error);
                }
                return;
            }
            (m, KeyCode::F(2)) if m.is_empty() || m.contains(KeyModifiers::SHIFT) => {
                self.open_table_detail_from_sidebar();
                return;
            }
            (m, KeyCode::F(3)) if m.is_empty() || m.contains(KeyModifiers::SHIFT) => {
                self.open_history();
                return;
            }
            (m, KeyCode::F(1)) | (m, KeyCode::Char('?'))
                if m.is_empty() || m.contains(KeyModifiers::SHIFT) =>
            {
                self.show_help = true;
                return;
            }
            _ => {}
        }

        match self.focus {
            Focus::Editor => self.on_key_editor(key),
            Focus::Results => self.on_key_results(key),
            Focus::Sidebar => self.on_key_sidebar(key),
        }
    }

    fn on_key_editor(&mut self, key: KeyEvent) {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let shift = key.modifiers.contains(KeyModifiers::SHIFT);
        if matches!(
            key.code,
            KeyCode::Up
                | KeyCode::Down
                | KeyCode::Left
                | KeyCode::Right
                | KeyCode::Home
                | KeyCode::End
                | KeyCode::PageUp
                | KeyCode::PageDown
        ) {
            if shift {
                self.editor.begin_selection();
            } else {
                self.editor.clear_selection();
            }
        }
        match key.code {
            KeyCode::Tab => {
                if self.popup.is_some() {
                    self.accept_suggestion();
                } else {
                    self.focus = match self.focus {
                        Focus::Editor => Focus::Results,
                        Focus::Results => Focus::Sidebar,
                        Focus::Sidebar => Focus::Editor,
                    };
                    if self.focus == Focus::Results
                        && !self.result_views.is_empty()
                        && self.table_state.selected().is_none()
                    {
                        self.table_state.select(Some(0));
                    }
                }
            }
            KeyCode::Esc => {
                if self.popup.is_some() {
                    self.popup = None;
                } else {
                    self.editor.clear_selection();
                }
            }
            KeyCode::Char('a') if ctrl => {
                self.editor.select_all();
                self.clamp_editor_scroll();
                self.popup = None;
            }
            KeyCode::Char('z' | 'Z') if ctrl && shift => {
                self.redo();
            }
            KeyCode::Char('z') if ctrl => {
                if self.editor.undo() {
                    self.clamp_editor_scroll();
                    self.popup = None;
                } else {
                    self.set_status("nothing to undo", StatusKind::Info);
                }
            }
            KeyCode::Char('y') if ctrl => {
                self.redo();
            }
            KeyCode::Char('x') if ctrl => {
                self.cut_selection();
            }
            KeyCode::Char('v') if ctrl => {
                self.paste_from_clipboard();
            }
            KeyCode::Char('p') if ctrl => {
                if !self.popup_navigate(true) {
                    self.history_move(true);
                }
            }
            KeyCode::Char('n') if ctrl => {
                if !self.popup_navigate(false) {
                    self.history_move(false);
                }
            }
            KeyCode::Char(' ') if ctrl => {
                self.update_popup();
            }
            KeyCode::Up => {
                self.editor.move_up();
                self.clamp_editor_scroll();
                self.update_popup();
            }
            KeyCode::Down => {
                self.editor.move_down();
                self.clamp_editor_scroll();
                self.update_popup();
            }
            KeyCode::Left => {
                self.editor.move_left();
                self.clamp_editor_scroll();
                self.update_popup();
            }
            KeyCode::Right => {
                self.editor.move_right();
                self.clamp_editor_scroll();
                self.update_popup();
            }
            KeyCode::Home if ctrl => {
                self.editor.move_doc_start();
                self.clamp_editor_scroll();
            }
            KeyCode::End if ctrl => {
                self.editor.move_doc_end();
                self.clamp_editor_scroll();
            }
            KeyCode::Home => {
                self.editor.move_home();
                self.clamp_editor_scroll();
            }
            KeyCode::End => {
                self.editor.move_end();
                self.clamp_editor_scroll();
            }
            KeyCode::PageUp => {
                for _ in 0..10 {
                    self.editor.move_up();
                }
                self.clamp_editor_scroll();
            }
            KeyCode::PageDown => {
                for _ in 0..10 {
                    self.editor.move_down();
                }
                self.clamp_editor_scroll();
            }
            KeyCode::Enter => {
                self.editor.newline();
                self.clamp_editor_scroll();
            }
            KeyCode::F(9) => {
                self.submit_query();
            }
            KeyCode::Backspace => {
                self.editor.backspace();
                self.clamp_editor_scroll();
                self.update_popup();
            }
            KeyCode::Delete => {
                self.editor.delete();
                self.update_popup();
            }
            KeyCode::Char(c) if !ctrl => {
                self.editor.insert_char(c);
                self.clamp_editor_scroll();
                self.update_popup();
            }
            _ => {}
        }
    }

    fn on_key_results(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Tab => {
                self.focus = Focus::Sidebar;
                return;
            }
            KeyCode::Esc | KeyCode::Char('i') => {
                self.focus = Focus::Editor;
                return;
            }
            KeyCode::Char('[') => {
                self.cycle_result(-1);
                return;
            }
            KeyCode::Char(']') => {
                self.cycle_result(1);
                return;
            }
            _ => {}
        }
        if let Some(err) = self.last_error.clone() {
            if matches!(key.code, KeyCode::Char('y') | KeyCode::Char('Y')) {
                self.copy_to_clipboard(err, "error");
            }
            return;
        }
        let Some(rv) = self.current_result() else {
            return;
        };
        let rows = rv.result.rows.len();
        let cols = rv.result.columns.len();
        self.selected_col = self.selected_col.min(cols.saturating_sub(1));
        match key.code {
            KeyCode::Enter | KeyCode::Char('v') => {
                self.open_cell_view();
            }
            KeyCode::Char('n') => {
                self.load_next_page();
            }
            KeyCode::Up | KeyCode::Char('k') => {
                let sel = self.table_state.selected().unwrap_or(0);
                if sel > 0 {
                    self.table_state.select(Some(sel - 1));
                }
            }
            KeyCode::Down | KeyCode::Char('j') => {
                let sel = self.table_state.selected().unwrap_or(0);
                if sel + 1 < rows {
                    self.table_state.select(Some(sel + 1));
                }
            }
            KeyCode::Char('y') => {
                self.copy_cell();
            }
            KeyCode::Char('Y') => {
                self.copy_row();
            }
            KeyCode::Home if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.first_col = 0;
                self.selected_col = 0;
            }
            KeyCode::Home => {
                if rows > 0 {
                    self.table_state.select(Some(0));
                }
            }
            KeyCode::End => {
                if rows > 0 {
                    self.table_state.select(Some(rows - 1));
                }
            }
            KeyCode::PageUp => {
                let sel = self.table_state.selected().unwrap_or(0);
                self.table_state.select(Some(sel.saturating_sub(10)));
            }
            KeyCode::PageDown => {
                let sel = self.table_state.selected().unwrap_or(0);
                if rows > 0 {
                    self.table_state.select(Some((sel + 10).min(rows - 1)));
                }
            }
            KeyCode::Left => {
                self.selected_col = self.selected_col.saturating_sub(1);
                self.ensure_col_visible();
            }
            KeyCode::Right => {
                if self.selected_col + 1 < cols {
                    self.selected_col += 1;
                }
                self.ensure_col_visible();
            }
            _ => {}
        }
    }

    /// Scroll the column window so the selected column is on screen.
    fn ensure_col_visible(&mut self) {
        let Some(rv) = self.current_result() else {
            return;
        };
        if self.first_col > self.selected_col
            || !col_visible(&rv.widths, self.first_col, self.selected_col, self.results_view_w)
        {
            self.first_col = self.selected_col;
        }
    }

    fn on_key_sidebar(&mut self, key: KeyEvent) {
        let mut items = self.sidebar_items();
        let n = items.len();
        if n > 0 {
            self.sidebar_selected = self.sidebar_selected.min(n - 1);
        }
        match key.code {
            KeyCode::Tab | KeyCode::Esc => {
                self.focus = Focus::Editor;
            }
            KeyCode::Up | KeyCode::Char('k') => {
                if n > 0 {
                    self.sidebar_selected = self.sidebar_selected.saturating_sub(1);
                }
            }
            KeyCode::Down | KeyCode::Char('j') => {
                if n > 0 && self.sidebar_selected + 1 < n {
                    self.sidebar_selected += 1;
                }
            }
            KeyCode::Home => {
                if n > 0 {
                    self.sidebar_selected = 0;
                }
            }
            KeyCode::End => {
                if n > 0 {
                    self.sidebar_selected = n - 1;
                }
            }
            KeyCode::PageUp => {
                if n > 0 {
                    self.sidebar_selected = self.sidebar_selected.saturating_sub(10).min(n - 1);
                }
            }
            KeyCode::PageDown => {
                if n > 0 {
                    self.sidebar_selected = (self.sidebar_selected + 10).min(n - 1);
                }
            }
            KeyCode::Right | KeyCode::Char('l') => {
                match items.get(self.sidebar_selected) {
                    Some(SidebarItem::Database(db)) => {
                        self.sidebar_expanded.insert(db.clone());
                    }
                    Some(SidebarItem::Table(db, table)) => {
                        let table_key = format!("{}.{}", db, table);
                        self.sidebar_tables_expanded.insert(table_key.clone());
                        self.maybe_fetch_table_detail(db.clone(), table.clone());
                    }
                    _ => {}
                }
            }
            KeyCode::Left | KeyCode::Char('h') => {
                if let Some(item) = items.get(self.sidebar_selected) {
                    match item {
                        SidebarItem::Database(db) => {
                            let _ = self.sidebar_expanded.remove(db);
                        }
                        SidebarItem::Table(db, table) => {
                            let table_key = format!("{}.{}", db, table);
                            if self.sidebar_tables_expanded.remove(&table_key) {
                                // Table collapsed, stay on table
                            } else if self.sidebar_expanded.remove(db) {
                                // Database collapsed, jump to database
                                if let Some(idx) = items.iter().position(|i| {
                                    matches!(i, SidebarItem::Database(d) if d == db)
                                }) {
                                    self.sidebar_selected = idx;
                                }
                            }
                        }
                        SidebarItem::Column(db, table, _) => {
                            // Navigate to parent table
                            let table_key = format!("{}.{}", db, table);
                            if self.sidebar_tables_expanded.remove(&table_key) {
                                // Jump to parent table
                                if let Some(idx) = items.iter().position(|i| {
                                    matches!(i, SidebarItem::Table(d, t) if d == db && t == table)
                                }) {
                                    self.sidebar_selected = idx;
                                }
                            }
                        }
                    }
                }
            }
            KeyCode::Enter if n > 0 => {
                match items.remove(self.sidebar_selected) {
                    SidebarItem::Database(db) => {
                        if self.busy.is_some() {
                            self.set_status(
                                "busy — wait for the current operation",
                                StatusKind::Error,
                            );
                            return;
                        }
                        self.busy = Some(Busy::Querying);
                        self.query_tab = self.tab_idx;
                        self.last_error = None;
                        self.set_status(format!("switching to `{db}`..."), StatusKind::Info);
                        self.worker.send(DbCommand::Query(format!("USE `{db}`")));
                    }
                    SidebarItem::Table(db, table) => {
                        let before = self.editor.line_before_cursor();
                        if before.ends_with(|c: char| !c.is_whitespace()) {
                            self.editor.insert_text(" ");
                        }
                        self.editor.insert_text(&format!("SELECT * FROM {db}.{table}"));
                        self.update_popup();
                        self.focus = Focus::Editor;
                    }
                    SidebarItem::Column(_, _, col) => {
                        let before = self.editor.line_before_cursor();
                        if before.ends_with(|c: char| !c.is_whitespace()) {
                            self.editor.insert_text(" ");
                        }
                        self.editor.insert_text(&col);
                        self.update_popup();
                        self.focus = Focus::Editor;
                    }
                }
            }
            _ => {}
        }
    }

    fn do_export(&mut self, path: &str, format: ExportFormat) {
        let Some(rv) = self.current_result() else {
            self.set_status("no result to export", StatusKind::Error);
            return;
        };
        let path = path.trim().to_string();
        if path.is_empty() {
            self.set_status("enter a file path first", StatusKind::Error);
            return;
        }
        match export(&rv.result, &path, format) {
            Ok((final_path, n)) => {
                self.export_dlg = None;
                self.set_status(
                    format!("exported {n} rows to {}", final_path.display()),
                    StatusKind::Ok,
                );
            }
            Err(e) => {
                self.set_status(e, StatusKind::Error);
            }
        }
    }

    fn do_export_all(&mut self, dlg: ExportDialog) {
        let path = dlg.path.trim().to_string();
        if path.is_empty() {
            self.set_status("enter a file path first", StatusKind::Error);
            return;
        }
        let sql = self.editor.text().trim().to_string();
        if sql.is_empty() {
            self.set_status("no query to export", StatusKind::Error);
            return;
        }
        self.busy = Some(Busy::Exporting);
        self.set_status("exporting all rows...", StatusKind::Info);
        self.worker.send(DbCommand::ExportAll {
            sql,
            path,
            format: dlg.format,
        });
    }

    pub fn on_paste(&mut self, text: String) {
        match self.mode {
            Mode::Query if self.focus == Focus::Editor && self.export_dlg.is_none() => {
                self.editor.insert_text(&text);
                self.clamp_editor_scroll();
                self.update_popup();
            }
            Mode::ConnForm => {
                if let Some(form) = self.form.as_mut() {
                    let text = text.replace(['\n', '\r'], " ");
                    let field = &mut form.fields[form.active];
                    field.value.insert_str(field.cursor, &text);
                    field.cursor += text.len();
                }
            }
            _ => {}
        }
    }
}

const DESTRUCTIVE_KEYWORDS: &[&str] = &["DELETE", "UPDATE", "DROP", "ALTER", "TRUNCATE"];

fn is_destructive(sql: &str) -> bool {
    for stmt in crate::db::split_statements(sql) {
        let first = stmt
            .split_whitespace()
            .next()
            .unwrap_or("")
            .to_uppercase();
        if DESTRUCTIVE_KEYWORDS.contains(&first.as_str()) {
            return true;
        }
    }
    false
}

/// Whether column `col` is inside the visible column window, mirroring
/// the fit logic of `ui::draw_results`.
fn col_visible(widths: &[u16], first_col: usize, col: usize, view_w: u16) -> bool {
    let width = view_w.saturating_sub(2) as usize;
    let mut used = 0usize;
    for ci in first_col..=col {
        let w = widths.get(ci).map(|w| *w as usize).unwrap_or(0);
        let extra = if ci == first_col { 0 } else { 2 };
        if ci != first_col && used + w + extra > width {
            return false;
        }
        used += w + extra;
    }
    true
}

fn apply_default_limit(sql: &str, limit: usize) -> String {
    if limit == 0 {
        return sql.to_string();
    }
    let stmts = crate::db::split_statements(sql);
    let mut result = Vec::with_capacity(stmts.len());
    for stmt in &stmts {
        let trimmed = stmt.trim_start();
        let upper = trimmed.to_uppercase();
        let is_select = upper.starts_with("SELECT");
        let has_limit = stmt
            .to_uppercase()
            .split_whitespace()
            .any(|w| w == "LIMIT");
        if is_select && !has_limit {
            result.push(format!("{} LIMIT {}", stmt.trim(), limit));
        } else {
            result.push(stmt.trim().to_string());
        }
    }
    result.join("; ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::SchemaCache;

    fn app() -> App {
        let mut a = App::new().unwrap();
        // App::new() may auto-connect from the real user config; reset for hermetic tests.
        a.config = Config::default();
        a.config_error = None;
        a.mode = Mode::ConnList;
        a.busy = None;
        a.conn_info = None;
        a.active_conn = None;
        a.history.clear();
        a.history_path = None;
        a
    }

    #[test]
    fn form_field_editing() {
        let mut f = FormField::new("Host", "127.0.0.1".into(), false);
        f.left();
        f.left();
        f.insert('2');
        assert_eq!(f.value, "127.0.02.1");
        f.backspace();
        assert_eq!(f.value, "127.0.0.1");
        f.cursor = f.value.len();
        f.backspace();
        f.backspace();
        assert_eq!(f.value, "127.0.0");
    }

    #[test]
    fn form_to_connection_validates_port() {
        let mut form = ConnForm::from_connection(None, &Connection::default());
        assert!(form.to_connection().is_ok());
        form.fields[2].value = "not-a-port".into();
        assert!(form.to_connection().is_err());
        form.fields[2].value = "3307".into();
        let c = form.to_connection().unwrap();
        assert_eq!(c.port, 3307);
    }

    #[test]
    fn form_ssh_fields_roundtrip() {
        let conn = Connection {
            host: "10.0.0.5".into(),
            ssh: Some(SshConfig {
                host: "bastion".into(),
                port: 2222,
                user: "deploy".into(),
                key_path: "~/.ssh/id_ed25519".into(),
            }),
            ..Connection::default()
        };
        let form = ConnForm::from_connection(None, &conn);
        let back = form.to_connection().unwrap();
        assert_eq!(back.ssh, conn.ssh);
        assert!(back.has_ssh());
    }

    #[test]
    fn form_ssh_defaults_and_validation() {
        let mut form = ConnForm::from_connection(None, &Connection::default());
        assert_eq!(form.to_connection().unwrap().ssh, None);
        form.fields[6].value = "jump".into();
        let ssh = form.to_connection().unwrap().ssh.expect("ssh present");
        assert_eq!(ssh.host, "jump");
        assert_eq!(ssh.port, 22);
        assert_eq!(ssh.user, "");
        form.fields[7].value = "not-a-port".into();
        assert!(form.to_connection().is_err());
    }

    #[test]
    fn secret_field_masked() {
        let f = FormField::new("Password", "hunter2".into(), true);
        assert_eq!(f.rendered(), "*******");
    }

    #[test]
    fn tls_choices_match_config_modes() {
        let expected: Vec<&str> = TlsMode::ALL.iter().map(|m| m.as_str()).collect();
        assert_eq!(TLS_CHOICES, expected.as_slice());
    }

    #[test]
    fn choice_field_cycles_and_ignores_text() {
        let mut f = FormField::choice("TLS", TLS_CHOICES, "off");
        f.right();
        assert_eq!(f.value, "required");
        f.left();
        f.left();
        assert_eq!(f.value, "verify-identity");
        f.right();
        assert_eq!(f.value, "off");
        f.insert('x');
        f.backspace();
        f.delete();
        assert_eq!(f.value, "off");
        // unknown stored value falls back to the first choice
        let f = FormField::choice("TLS", TLS_CHOICES, "bogus");
        assert_eq!(f.value, "off");
    }

    #[test]
    fn form_tls_roundtrip_and_validation() {
        let conn = Connection {
            tls: TlsConfig {
                mode: TlsMode::VerifyIdentity,
                ca_path: "~/ca.pem".into(),
            },
            ..Connection::default()
        };
        let form = ConnForm::from_connection(None, &conn);
        assert_eq!(form.fields[10].value, "verify-identity");
        assert_eq!(form.to_connection().unwrap().tls, conn.tls);

        let mut form = ConnForm::from_connection(None, &Connection::default());
        assert_eq!(form.to_connection().unwrap().tls.mode, TlsMode::Off);
        form.fields[11].value = "/tmp/ca.pem".into();
        assert!(form.to_connection().is_err(), "CA without TLS is rejected");
        form.fields[10].right();
        let tls = form.to_connection().unwrap().tls;
        assert_eq!(tls.mode, TlsMode::Required);
        assert_eq!(tls.ca_path, "/tmp/ca.pem");
    }

    #[test]
    fn result_view_widths() {
        let rv = ResultView::new(QueryResult {
            columns: vec!["id".into(), "long_column_name".into()],
            rows: vec![vec![
                mysql::Value::Int(1),
                mysql::Value::Bytes(b"xy".to_vec()),
            ]],
            truncated: false,
            elapsed: std::time::Duration::ZERO,
            source: None,
            has_more: false,
        });
        assert_eq!(rv.widths[0], 4);
        assert_eq!(rv.widths[1], "long_column_name".len() as u16);
        assert_eq!(rv.display[0][1], "xy");
    }

    fn sample_result(name: &str) -> QueryResult {
        QueryResult {
            columns: vec![name.into()],
            rows: vec![vec![mysql::Value::Int(1)]],
            truncated: false,
            elapsed: std::time::Duration::ZERO,
            source: None,
            has_more: false,
        }
    }

    #[test]
    fn multi_result_done_event_populates_views() {
        let mut a = app();
        a.on_db_event(DbEvent::QueryDone {
            results: vec![sample_result("a"), sample_result("b")],
            message: "2 rows in 2 result sets".into(),
        });
        assert_eq!(a.result_views.len(), 2);
        assert_eq!(a.result_idx, 0);
        assert!(a.status.0.contains("2 result sets"));
    }

    #[test]
    fn multi_result_cycling() {
        let mut a = app();
        a.mode = Mode::Query;
        a.focus = Focus::Results;
        a.result_views = vec![
            ResultView::new(sample_result("a")),
            ResultView::new(sample_result("b")),
        ];
        a.first_col = 3;
        a.on_key(KeyEvent::new(KeyCode::Char(']'), KeyModifiers::NONE));
        assert_eq!(a.result_idx, 1);
        assert_eq!(a.first_col, 0);
        a.on_key(KeyEvent::new(KeyCode::Char(']'), KeyModifiers::NONE));
        assert_eq!(a.result_idx, 0, "cycling wraps around");
        a.on_key(KeyEvent::new(KeyCode::Char('['), KeyModifiers::NONE));
        assert_eq!(a.result_idx, 1, "wraps backwards too");
    }

    #[test]
    fn single_result_set_ignores_cycle_keys() {
        let mut a = app();
        a.mode = Mode::Query;
        a.focus = Focus::Results;
        a.result_views = vec![ResultView::new(sample_result("a"))];
        a.on_key(KeyEvent::new(KeyCode::Char(']'), KeyModifiers::NONE));
        assert_eq!(a.result_idx, 0);
    }

    fn wide_result() -> ResultView {
        ResultView::new(QueryResult {
            columns: vec!["id".into(), "name".into(), "email".into()],
            rows: vec![vec![
                mysql::Value::Int(1),
                mysql::Value::Bytes(b"alice".to_vec()),
                mysql::Value::Bytes(b"a@x.io".to_vec()),
            ]],
            truncated: false,
            elapsed: std::time::Duration::ZERO,
            source: None,
            has_more: false,
        })
    }

    #[test]
    fn copy_cell_and_row_text() {
        let mut a = app();
        a.mode = Mode::Query;
        a.focus = Focus::Results;
        a.result_views = vec![wide_result()];
        a.table_state.select(Some(0));
        assert_eq!(a.cell_text().as_deref(), Some("1"));
        assert_eq!(a.row_text().as_deref(), Some("1\talice\ta@x.io"));
        a.on_key(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE));
        assert_eq!(a.cell_text().as_deref(), Some("alice"));
        a.on_key(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE));
        a.on_key(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE)); // stays on last column
        assert_eq!(a.cell_text().as_deref(), Some("a@x.io"));
        a.on_key(KeyEvent::new(KeyCode::Left, KeyModifiers::NONE));
        assert_eq!(a.cell_text().as_deref(), Some("alice"));
    }

    #[test]
    fn column_cursor_visibility_follows() {
        let mut a = app();
        a.mode = Mode::Query;
        a.focus = Focus::Results;
        a.result_views = vec![wide_result()];
        a.table_state.select(Some(0));
        // narrow viewport: only one column fits
        a.results_view_w = 8;
        a.on_key(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE));
        assert_eq!(a.selected_col, 1);
        assert_eq!(a.first_col, 1, "cursor scrolls into view as leftmost");
        a.on_key(KeyEvent::new(KeyCode::Left, KeyModifiers::NONE));
        assert_eq!(a.selected_col, 0);
        assert_eq!(a.first_col, 0);
    }

    #[test]
    fn column_cursor_resets_on_new_results() {
        let mut a = app();
        a.selected_col = 5;
        a.on_db_event(DbEvent::QueryDone {
            results: vec![sample_result("a")],
            message: "1 row".into(),
        });
        assert_eq!(a.selected_col, 0);
    }

    #[test]
    fn cancel_without_connection_info_errors() {
        let mut a = app();
        a.mode = Mode::Query;
        a.focus = Focus::Editor;
        a.busy = Some(Busy::Querying);
        a.on_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert!(a.status.0.contains("cannot cancel"), "got: {}", a.status.0);
        assert!(matches!(a.busy, Some(Busy::Querying)));
    }

    #[test]
    fn query_failed_keeps_full_error_and_short_status() {
        let mut a = app();
        a.busy = Some(Busy::Querying);
        let msg = "ERROR 1064 (42000): You have an error in your SQL syntax\n\nin: SELEC 1";
        a.on_db_event(DbEvent::QueryFailed(msg.into()));
        assert!(a.busy.is_none());
        assert_eq!(a.last_error.as_deref(), Some(msg));
        assert_eq!(a.status.0, "ERROR 1064 (42000): You have an error in your SQL syntax");
        assert_eq!(a.status.1, StatusKind::Error);

        // a new query dismisses the error pane
        a.editor = Editor::from_text("SELECT 1");
        a.submit_query();
        assert!(a.last_error.is_none());
    }

    #[test]
    fn query_cancelled_is_not_an_error_pane() {
        let mut a = app();
        a.busy = Some(Busy::Querying);
        a.on_db_event(DbEvent::QueryFailed("query cancelled".into()));
        assert!(a.last_error.is_none());
        assert_eq!(a.status.1, StatusKind::Info);
    }

    #[test]
    fn error_pane_swallows_result_navigation() {
        let mut a = app();
        a.mode = Mode::Query;
        a.on_db_event(DbEvent::QueryDone {
            results: vec![sample_result("a")],
            message: "1 row".into(),
        });
        a.on_db_event(DbEvent::QueryFailed("ERROR 1146 (42S02): Table 'x' doesn't exist".into()));
        a.focus = Focus::Results;
        let before = a.table_state.selected();
        a.on_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        assert_eq!(a.table_state.selected(), before);
        a.on_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert_eq!(a.focus, Focus::Editor);
    }

    #[test]
    fn editor_selection_keys_and_undo() {
        let mut a = app();
        a.mode = Mode::Query;
        a.focus = Focus::Editor;
        a.editor = Editor::from_text("SELECT 1");
        let key = |c: KeyCode, m: KeyModifiers| KeyEvent::new(c, m);

        for _ in 0..6 {
            a.on_key(key(KeyCode::Right, KeyModifiers::SHIFT));
        }
        assert_eq!(a.editor.selected_text().as_deref(), Some("SELECT"));
        a.on_key(key(KeyCode::Left, KeyModifiers::NONE));
        assert!(!a.editor.has_selection());

        a.on_key(key(KeyCode::Char('a'), KeyModifiers::CONTROL));
        assert_eq!(a.editor.selected_text().as_deref(), Some("SELECT 1"));
        a.on_key(key(KeyCode::Char('x'), KeyModifiers::NONE));
        assert_eq!(a.editor.text(), "x");
        a.on_key(key(KeyCode::Char('z'), KeyModifiers::CONTROL));
        assert_eq!(a.editor.text(), "SELECT 1");
        a.on_key(key(KeyCode::Char('y'), KeyModifiers::CONTROL));
        assert_eq!(a.editor.text(), "x");
        a.on_key(key(KeyCode::Char('z'), KeyModifiers::CONTROL));
        a.on_key(key(KeyCode::Char('Z'), KeyModifiers::CONTROL | KeyModifiers::SHIFT));
        assert_eq!(a.editor.text(), "x");
    }

    #[test]
    fn ctrl_c_copies_selection_instead_of_quitting() {
        let mut a = app();
        a.mode = Mode::Query;
        a.focus = Focus::Editor;
        a.editor = Editor::from_text("abc");
        a.on_key(KeyEvent::new(KeyCode::Right, KeyModifiers::SHIFT));
        assert!(a.editor.has_selection());
        a.on_key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL));
        assert!(!a.should_quit);
        // status reports either a copy or a clipboard error in headless CI
        assert!(!a.status.0.is_empty());
        a.editor.clear_selection();
        a.on_key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL));
        assert!(a.should_quit);
    }

    #[test]
    fn history_persists_on_submit_and_delete() {
        let mut dir = std::env::temp_dir();
        dir.push(format!("mysql-tui-app-hist-{}", std::process::id()));
        let path = dir.join("history.json");
        let mut a = app();
        a.history_path = Some(path.clone());
        a.editor = Editor::from_text("SELECT 1");
        a.submit_query();
        a.busy = None;
        a.editor = Editor::from_text("SELECT 2");
        a.submit_query();
        assert_eq!(crate::history::load(&path).unwrap(), vec!["SELECT 1", "SELECT 2"]);

        a.busy = None;
        a.mode = Mode::Query;
        a.open_history();
        a.on_key(KeyEvent::new(KeyCode::Char('d'), KeyModifiers::CONTROL)); // newest first
        assert_eq!(crate::history::load(&path).unwrap(), vec!["SELECT 1"]);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    fn mixed_result() -> QueryResult {
        QueryResult {
            columns: vec!["id".into(), "payload".into(), "note".into()],
            rows: vec![
                vec![
                    mysql::Value::Int(1),
                    mysql::Value::Bytes(br#"{"a":1,"b":[1,2]}"#.to_vec()),
                    mysql::Value::Bytes("line1\nline2".as_bytes().to_vec()),
                ],
                vec![
                    mysql::Value::Int(2),
                    mysql::Value::Bytes(b"plain text".to_vec()),
                    mysql::Value::NULL,
                ],
            ],
            truncated: false,
            elapsed: std::time::Duration::ZERO,
            source: None,
            has_more: false,
        }
    }

    fn app_with_result() -> App {
        let mut a = app();
        a.mode = Mode::Query;
        a.focus = Focus::Results;
        a.on_db_event(DbEvent::QueryDone {
            results: vec![mixed_result()],
            message: "2 rows".into(),
        });
        a
    }

    #[test]
    fn cell_view_detects_and_toggles_json() {
        let mut v = CellView::new("payload".into(), 0, r#"{"a":1,"b":[1,2]}"#.into());
        assert!(v.pretty.is_some());
        assert!(v.show_pretty);
        assert!(v.text().contains("\n  \"a\": 1"));
        v.scroll = 5;
        assert!(v.toggle_pretty());
        assert_eq!(v.text(), r#"{"a":1,"b":[1,2]}"#);
        assert_eq!(v.scroll, 0);

        let mut plain = CellView::new("note".into(), 0, "plain text".into());
        assert!(plain.pretty.is_none());
        assert!(!plain.toggle_pretty());
        // a bare JSON scalar is not worth pretty-printing
        assert!(CellView::new("n".into(), 0, "42".into()).pretty.is_none());
        assert!(CellView::new("n".into(), 0, "[not json".into()).pretty.is_none());
    }

    #[test]
    fn enter_opens_cell_view_and_keys_navigate_cells() {
        let mut a = app_with_result();
        a.on_key(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE));
        a.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        let v = a.cell_view.as_ref().expect("viewer open");
        assert_eq!(v.column, "payload");
        assert_eq!(v.row, 0);
        assert!(v.show_pretty);
        assert!(a.query_dialog_open());

        // J moves to the next row and reloads the viewer
        a.on_key(KeyEvent::new(KeyCode::Char('J'), KeyModifiers::SHIFT));
        let v = a.cell_view.as_ref().unwrap();
        assert_eq!((v.row, v.column.as_str(), v.raw.as_str()), (1, "payload", "plain text"));
        assert_eq!(a.table_state.selected(), Some(1));

        // Right moves to the next column; past the last column is a no-op
        a.on_key(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE));
        assert_eq!(a.cell_view.as_ref().unwrap().raw, "NULL");
        a.on_key(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE));
        assert_eq!(a.selected_col, 2);

        // scroll keys never go negative; End is clamped by the renderer
        a.on_key(KeyEvent::new(KeyCode::Char('k'), KeyModifiers::NONE));
        assert_eq!(a.cell_view.as_ref().unwrap().scroll, 0);
        a.on_key(KeyEvent::new(KeyCode::End, KeyModifiers::NONE));
        assert_eq!(a.cell_view.as_ref().unwrap().scroll, usize::MAX);

        // keys are swallowed by the popup: 'i' must not change focus
        a.on_key(KeyEvent::new(KeyCode::Char('i'), KeyModifiers::NONE));
        assert_eq!(a.focus, Focus::Results);
        a.on_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert!(a.cell_view.is_none());
        assert_eq!(a.focus, Focus::Results, "closing the viewer keeps results focus");
    }

    #[test]
    fn cell_view_closes_on_new_result_and_tab_switch() {
        let mut a = app_with_result();
        a.on_key(KeyEvent::new(KeyCode::Char('v'), KeyModifiers::NONE));
        assert!(a.cell_view.is_some());
        a.on_db_event(DbEvent::QueryDone {
            results: vec![sample_result("x")],
            message: "1 row".into(),
        });
        assert!(a.cell_view.is_none());

        a.on_key(KeyEvent::new(KeyCode::Char('v'), KeyModifiers::NONE));
        assert!(a.cell_view.is_some());
        a.new_tab();
        assert!(a.cell_view.is_none());

        a.focus = Focus::Results;
        a.on_key(KeyEvent::new(KeyCode::Char('v'), KeyModifiers::NONE));
        assert!(a.cell_view.is_none(), "no result in the new tab");
    }

    #[test]
    fn result_widths_use_display_width_and_first_line() {
        let rv = ResultView::new(QueryResult {
            columns: vec!["c".into()],
            rows: vec![
                vec![mysql::Value::Bytes("中文".as_bytes().to_vec())],
                vec![mysql::Value::Bytes("ab\nlong second line".as_bytes().to_vec())],
            ],
            truncated: false,
            elapsed: std::time::Duration::ZERO,
            source: None,
            has_more: false,
        });
        assert_eq!(rv.widths, vec![4]); // "中文" is 4 cells wide, second row counts only "ab"
    }

    #[test]
    fn tab_title_formats_first_line() {
        assert_eq!(tab_title(""), "untitled");
        assert_eq!(tab_title("  \n\n"), "untitled");
        assert_eq!(tab_title("\n  select   *\n from t"), "select *");
        let long = tab_title("SELECT id, name, email FROM customers WHERE 1");
        assert_eq!(long.chars().count(), TAB_TITLE_WIDTH);
        assert!(long.ends_with('\u{2026}'));
    }

    #[test]
    fn tabs_keep_independent_editor_and_results() {
        let mut a = app();
        a.mode = Mode::Query;
        a.editor = Editor::from_text("SELECT 1");
        a.on_db_event(DbEvent::QueryDone {
            results: vec![sample_result("a")],
            message: "1 row".into(),
        });
        assert_eq!(a.result_views.len(), 1);

        a.on_key(KeyEvent::new(KeyCode::Char('t'), KeyModifiers::CONTROL));
        assert_eq!(a.tabs.len(), 2);
        assert_eq!(a.tab_idx, 1);
        assert_eq!(a.editor.text(), "");
        assert!(a.result_views.is_empty());
        assert_eq!(a.focus, Focus::Editor);

        a.editor = Editor::from_text("SELECT 2");
        a.on_key(KeyEvent::new(KeyCode::Left, KeyModifiers::ALT));
        assert_eq!(a.tab_idx, 0);
        assert_eq!(a.editor.text(), "SELECT 1");
        assert_eq!(a.result_views.len(), 1);

        a.on_key(KeyEvent::new(KeyCode::Right, KeyModifiers::ALT));
        assert_eq!(a.tab_idx, 1);
        assert_eq!(a.editor.text(), "SELECT 2");
        assert!(a.result_views.is_empty());

        // cycling wraps around
        a.on_key(KeyEvent::new(KeyCode::PageDown, KeyModifiers::CONTROL));
        assert_eq!(a.tab_idx, 0);

        let titles = a.tab_titles();
        assert_eq!(titles.len(), 2);
        assert_eq!(titles[0].0, "SELECT 1");
        assert_eq!(titles[1].0, "SELECT 2");
    }

    #[test]
    fn alt_digit_switches_tab() {
        let mut a = app();
        a.mode = Mode::Query;
        a.new_tab();
        a.new_tab();
        a.on_key(KeyEvent::new(KeyCode::Char('1'), KeyModifiers::ALT));
        assert_eq!(a.tab_idx, 0);
        a.on_key(KeyEvent::new(KeyCode::Char('3'), KeyModifiers::ALT));
        assert_eq!(a.tab_idx, 2);
        a.on_key(KeyEvent::new(KeyCode::Char('9'), KeyModifiers::ALT));
        assert_eq!(a.tab_idx, 2);
        assert!(a.status.0.contains("no tab 9"), "got: {}", a.status.0);
    }

    #[test]
    fn close_tab_guards_and_adjusts_query_tab() {
        let mut a = app();
        a.mode = Mode::Query;
        a.on_key(KeyEvent::new(KeyCode::Char('w'), KeyModifiers::CONTROL));
        assert_eq!(a.tabs.len(), 1);
        assert!(a.status.0.contains("cannot close the last tab"), "got: {}", a.status.0);

        a.editor = Editor::from_text("SELECT 1");
        a.new_tab();
        a.editor = Editor::from_text("SELECT 2");
        a.new_tab();
        a.editor = Editor::from_text("SELECT 3");
        a.submit_query(); // tab 3 (idx 2) is now running
        assert_eq!(a.query_tab, 2);

        a.on_key(KeyEvent::new(KeyCode::Char('w'), KeyModifiers::CONTROL));
        assert_eq!(a.tabs.len(), 3);
        assert!(a.status.0.contains("a query is running"), "got: {}", a.status.0);

        a.switch_tab(1);
        a.close_tab();
        assert_eq!(a.tabs.len(), 2);
        assert_eq!(a.query_tab, 1, "running tab index shifts down after closing an earlier tab");
        assert_eq!(a.tab_idx, 1);
        assert_eq!(a.editor.text(), "SELECT 3");

        a.switch_tab(0);
        assert_eq!(a.editor.text(), "SELECT 1");
        assert!(a.query_running_in(1));
        assert!(!a.query_running_in(0));
    }

    #[test]
    fn results_route_to_the_tab_that_ran_the_query() {
        let mut a = app();
        a.mode = Mode::Query;
        a.editor = Editor::from_text("SELECT 1");
        a.submit_query();
        a.new_tab();
        assert_eq!(a.tab_idx, 1);
        assert_eq!(a.query_tab, 0);

        a.on_db_event(DbEvent::QueryDone {
            results: vec![sample_result("a")],
            message: "1 row".into(),
        });
        assert!(a.busy.is_none());
        assert!(a.result_views.is_empty(), "active tab must not receive foreign results");
        assert_eq!(a.tabs[0].result_views.len(), 1);
        assert!(a.status.0.starts_with("[tab 1]"), "got: {}", a.status.0);

        a.switch_tab(0);
        assert_eq!(a.result_views.len(), 1);

        // same for errors
        a.submit_query();
        a.switch_tab(1);
        a.on_db_event(DbEvent::QueryFailed("ERROR 1064 (42000): bad\n\nin: x".into()));
        assert!(a.last_error.is_none());
        assert_eq!(a.tabs[0].last_error.as_deref(), Some("ERROR 1064 (42000): bad\n\nin: x"));
        assert!(a.status.0.starts_with("[tab 1] ERROR 1064"), "got: {}", a.status.0);
        a.switch_tab(0);
        assert!(a.last_error.is_some());
    }

    #[test]
    fn history_navigation() {
        let mut a = app();
        a.editor = Editor::from_text("SELECT 1");
        a.submit_query();
        a.busy = None; // simulate the worker reporting completion
        a.editor = Editor::from_text("SELECT 2");
        a.submit_query();
        a.history_move(true);
        assert_eq!(a.editor.text(), "SELECT 2");
        a.history_move(true);
        assert_eq!(a.editor.text(), "SELECT 1");
        a.history_move(false);
        assert_eq!(a.editor.text(), "SELECT 2");
    }

    #[test]
    fn history_dedupes_consecutive() {
        let mut a = app();
        a.editor = Editor::from_text("SELECT 1");
        a.submit_query();
        a.busy = None;
        a.editor = Editor::from_text("SELECT 1");
        a.submit_query();
        assert_eq!(a.history.len(), 1);
    }

    #[test]
    fn popup_updates_on_typing() {
        let mut a = app();
        let schema = SchemaCache::test_new("db", &[("users", &["id", "name"])], &["db"]);
        a.completer.update_schema(schema);
        for c in "SELECT * FROM us".chars() {
            a.editor.insert_char(c);
        }
        a.update_popup();
        let popup = a.popup.as_ref().expect("popup should open");
        assert_eq!(popup.suggestions[0].label, "users");
        // accept with Tab semantics
        a.accept_suggestion();
        assert_eq!(a.editor.text(), "SELECT * FROM users");
    }

    #[test]
    fn popup_navigates_then_accepts() {
        let mut a = app();
        let schema =
            SchemaCache::test_new("db", &[("users", &["id"]), ("user_logs", &["id"])], &[]);
        a.completer.update_schema(schema);
        a.editor.insert_text("SELECT * FROM user");
        a.update_popup();
        assert!(a.popup.is_some());
        a.popup_navigate(true); // wrap to last
        let sel = a.popup.as_ref().unwrap().selected;
        let expected = a.popup.as_ref().unwrap().suggestions[sel].label.clone();
        a.accept_suggestion();
        assert_eq!(a.editor.text(), format!("SELECT * FROM {expected}"));
    }

    #[test]
    fn clamp_editor_scroll_follows_cursor() {
        let mut a = app();
        a.editor_view = (10, 3);
        a.editor = Editor::from_text("line1\nline2\nline3\nline4\nline5");
        a.editor.move_doc_end();
        a.clamp_editor_scroll();
        // cursor at line 4 (0-indexed), 3-row viewport -> offset 2
        assert_eq!(a.editor_scroll.0, 2);
    }

    #[test]
    fn delete_connection_adjusts_indices() {
        let mut a = app();
        a.config.connections = vec![
            Connection {
                name: "a".into(),
                ..Connection::default()
            },
            Connection {
                name: "b".into(),
                ..Connection::default()
            },
        ];
        a.config.auto_connect = Some(1);
        a.conn_list_selected = 1;
        a.delete_connection(0);
        assert_eq!(a.config.connections.len(), 1);
        assert_eq!(a.config.connections[0].name, "b");
        assert_eq!(a.config.auto_connect, Some(0));
        assert_eq!(a.conn_list_selected, 0);
    }

    #[test]
    fn destructive_detection() {
        assert!(is_destructive("DELETE FROM users WHERE id = 1"));
        assert!(is_destructive("  update users set name='x'"));
        assert!(is_destructive("DROP TABLE foo"));
        assert!(is_destructive("ALTER TABLE foo ADD COLUMN bar INT"));
        assert!(is_destructive("TRUNCATE TABLE logs"));
        assert!(!is_destructive("SELECT * FROM users"));
        assert!(!is_destructive("SHOW TABLES"));
        assert!(!is_destructive("INSERT INTO users VALUES (1)"));
        assert!(is_destructive("SELECT 1; DELETE FROM users"));
    }

    #[test]
    fn default_limit_applied() {
        assert_eq!(
            apply_default_limit("SELECT * FROM users", 1000),
            "SELECT * FROM users LIMIT 1000"
        );
        assert_eq!(
            apply_default_limit("SELECT * FROM users LIMIT 10", 1000),
            "SELECT * FROM users LIMIT 10"
        );
        assert_eq!(
            apply_default_limit("select id from t", 500),
            "select id from t LIMIT 500"
        );
        assert_eq!(
            apply_default_limit("DELETE FROM users", 1000),
            "DELETE FROM users"
        );
        assert_eq!(
            apply_default_limit("SELECT * FROM users", 0),
            "SELECT * FROM users"
        );
        assert_eq!(
            apply_default_limit("SELECT 1; SELECT 2", 100),
            "SELECT 1 LIMIT 100; SELECT 2 LIMIT 100"
        );
    }

    #[test]
    fn password_setup_field_switching() {
        let mut a = app();
        a.mode = Mode::Password;
        a.password_state = Some(PasswordState {
            mode: PasswordMode::Setup,
            password: String::new(),
            confirm: String::new(),
            active: 0,
            salt: String::new(),
            nonce: String::new(),
            ciphertext: String::new(),
            error: String::new(),
        });
        // Type in password field
        a.on_key_password(KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE));
        a.on_key_password(KeyEvent::new(KeyCode::Char('b'), KeyModifiers::NONE));
        let ps = a.password_state.as_ref().unwrap();
        assert_eq!(ps.password, "ab");
        assert_eq!(ps.confirm, "");
        // Switch to confirm field
        a.on_key_password(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
        a.on_key_password(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::NONE));
        let ps = a.password_state.as_ref().unwrap();
        assert_eq!(ps.password, "ab");
        assert_eq!(ps.confirm, "c");
    }

    #[test]
    fn password_mismatch_shows_error() {
        let mut a = app();
        a.mode = Mode::Password;
        a.password_state = Some(PasswordState {
            mode: PasswordMode::Setup,
            password: "foo".to_string(),
            confirm: "bar".to_string(),
            active: 0,
            salt: String::new(),
            nonce: String::new(),
            ciphertext: String::new(),
            error: String::new(),
        });
        a.on_key_password(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        let ps = a.password_state.as_ref().unwrap();
        assert_eq!(ps.error, "passwords do not match");
        assert_eq!(ps.active, 1); // focus moved to confirm
    }

    #[test]
    fn filtered_history_newest_first_and_filter() {
        let mut a = app();
        a.history = vec![
            "select 1".into(),
            "select * from users".into(),
            "UPDATE t SET x = 1".into(),
        ];
        // no filter: newest first
        assert_eq!(a.filtered_history(), vec![2, 1, 0]);
        a.history_filter = "SELECT".into();
        // case-insensitive substring, newest first
        assert_eq!(a.filtered_history(), vec![1, 0]);
        a.history_filter = "users".into();
        assert_eq!(a.filtered_history(), vec![1]);
        a.history_filter = "nope".into();
        assert!(a.filtered_history().is_empty());
    }

    #[test]
    fn history_panel_open_filter_navigate_load() {
        let mut a = app();
        a.mode = Mode::Query;
        a.history = vec![
            "select 1".into(),
            "select * from users".into(),
            "update t set x = 1".into(),
        ];
        a.on_key(KeyEvent::new(KeyCode::F(3), KeyModifiers::NONE));
        assert!(a.history_open);
        // type filter "sel" -> matches both selects, newest first
        a.on_key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::NONE));
        a.on_key(KeyEvent::new(KeyCode::Char('e'), KeyModifiers::NONE));
        a.on_key(KeyEvent::new(KeyCode::Char('l'), KeyModifiers::NONE));
        assert_eq!(a.history_filter, "sel");
        assert_eq!(a.filtered_history(), vec![1, 0]);
        assert_eq!(a.history_selected, 0);
        // move down to "select 1", load it
        a.on_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        assert_eq!(a.history_selected, 1);
        a.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(!a.history_open);
        assert_eq!(a.editor.text(), "select 1");
        assert_eq!(a.focus, Focus::Editor);
    }

    #[test]
    fn history_panel_delete_and_close() {
        let mut a = app();
        a.mode = Mode::Query;
        a.history = vec!["select 1".into(), "select 2".into()];
        a.on_key(KeyEvent::new(KeyCode::F(3), KeyModifiers::NONE));
        // newest first: selected entry is "select 2"
        a.on_key(KeyEvent::new(
            KeyCode::Char('d'),
            KeyModifiers::CONTROL,
        ));
        assert_eq!(a.history, vec!["select 1".to_string()]);
        assert!(a.history_open, "panel stays open after delete");
        a.on_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert!(!a.history_open);
    }
}

#[cfg(test)]
mod paging_tests {
    use super::*;
    use crate::config::Config;
    use crate::editor::Editor;
    use crate::schema::SchemaCache;

    fn app() -> App {
        let mut a = App::new().unwrap();
        a.config = Config::default();
        a.completer.update_schema(SchemaCache::default());
        a.mode = Mode::Query;
        a.focus = Focus::Results;
        a
    }

    fn page_result(n: usize, source: &str, has_more: bool) -> QueryResult {
        QueryResult {
            columns: vec!["id".into(), "name".into()],
            rows: (0..n)
                .map(|i| vec![Value::Int(i as i64), Value::Bytes(b"ab".to_vec())])
                .collect(),
            truncated: false,
            elapsed: std::time::Duration::ZERO,
            source: Some(source.into()),
            has_more,
        }
    }

    fn key(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE)
    }

    #[test]
    fn append_extends_rows_display_and_widths() {
        let mut rv = ResultView::new(page_result(2, "SELECT * FROM t LIMIT 2", true));
        assert_eq!(rv.widths, vec![4, 4]);
        rv.append(
            vec![vec![Value::Int(2), Value::Bytes(b"a much longer name".to_vec())]],
            false,
        );
        assert_eq!(rv.result.rows.len(), 3);
        assert_eq!(rv.display.len(), 3);
        assert_eq!(rv.display[2][1], "a much longer name");
        assert_eq!(rv.widths, vec![4, 18]);
        assert!(!rv.result.has_more);
        assert!(!rv.result.truncated);
    }

    #[test]
    fn n_key_requests_next_page_and_page_done_appends() {
        let mut a = app();
        a.on_db_event(DbEvent::QueryDone {
            results: vec![page_result(2, "SELECT * FROM t LIMIT 2", true)],
            message: "2 rows".into(),
        });
        a.table_state.select(Some(1));
        a.on_key(key('n'));
        assert!(matches!(a.busy, Some(Busy::Querying)));
        assert_eq!(a.query_tab, 0);
        assert_eq!(a.page_result_idx, 0);
        assert!(a.status.0.starts_with("loading next 2 rows"), "got: {}", a.status.0);

        // a second press while loading is refused
        a.on_key(key('n'));
        assert!(a.status.0.starts_with("busy"), "got: {}", a.status.0);

        a.on_db_event(DbEvent::PageDone {
            rows: vec![
                vec![Value::Int(2), Value::Bytes(b"c".to_vec())],
                vec![Value::Int(3), Value::Bytes(b"d".to_vec())],
            ],
            has_more: true,
            elapsed: std::time::Duration::from_millis(5),
        });
        assert!(a.busy.is_none());
        let rv = a.current_result().unwrap();
        assert_eq!(rv.result.rows.len(), 4);
        assert!(rv.result.has_more);
        assert_eq!(
            rv.result.next_page(),
            Some(("SELECT * FROM t LIMIT 2 OFFSET 4".into(), 2))
        );
        assert_eq!(a.table_state.selected(), Some(1), "selection is preserved");
        assert!(a.status.0.starts_with("loaded 2 more rows (4 total)"), "got: {}", a.status.0);

        // last page: fewer rows than the page size ends paging
        a.on_key(key('n'));
        a.on_db_event(DbEvent::PageDone {
            rows: vec![vec![Value::Int(4), Value::Bytes(b"e".to_vec())]],
            has_more: false,
            elapsed: std::time::Duration::ZERO,
        });
        assert_eq!(a.current_result().unwrap().result.rows.len(), 5);
        assert!(!a.current_result().unwrap().result.has_more);
        a.on_key(key('n'));
        assert!(a.busy.is_none());
        assert_eq!(a.status.0, "no more rows to load");
    }

    #[test]
    fn page_failed_keeps_rows_and_reports() {
        let mut a = app();
        a.on_db_event(DbEvent::QueryDone {
            results: vec![page_result(3, "SELECT * FROM t LIMIT 3", true)],
            message: "3 rows".into(),
        });
        a.on_key(key('n'));
        a.on_db_event(DbEvent::PageFailed("ERROR 1146 (42S02): Table 't' doesn't exist".into()));
        assert!(a.busy.is_none());
        assert!(a.last_error.is_none(), "existing rows must stay visible");
        assert_eq!(a.current_result().unwrap().result.rows.len(), 3);
        assert!(a.status.0.starts_with("load more failed: ERROR 1146"), "got: {}", a.status.0);
        assert!(matches!(a.status.1, StatusKind::Error));

        a.on_key(key('n'));
        a.on_db_event(DbEvent::PageFailed("query cancelled".into()));
        assert_eq!(a.status.0, "query cancelled");
    }

    #[test]
    fn page_done_routes_to_the_tab_and_set_that_requested_it() {
        let mut a = app();
        a.on_db_event(DbEvent::QueryDone {
            results: vec![
                page_result(1, "SELECT 1", false),
                page_result(2, "SELECT * FROM t LIMIT 2", true),
            ],
            message: "3 rows in 2 result sets".into(),
        });
        a.on_key(key(']'));
        assert_eq!(a.result_idx, 1);
        a.on_key(key('n'));
        assert_eq!(a.page_result_idx, 1);

        a.new_tab();
        a.editor = Editor::from_text("x");
        a.on_db_event(DbEvent::PageDone {
            rows: vec![vec![Value::Int(9), Value::NULL]],
            has_more: false,
            elapsed: std::time::Duration::ZERO,
        });
        assert!(a.result_views.is_empty(), "active tab must not receive foreign rows");
        assert_eq!(a.tabs[0].result_views[0].result.rows.len(), 1);
        assert_eq!(a.tabs[0].result_views[1].result.rows.len(), 3);
        assert!(a.status.0.starts_with("[tab 1] loaded 1 more row (3 total)"), "got: {}", a.status.0);

        // result set gone (new query replaced it) -> batch is dropped, no panic
        a.switch_tab(0);
        a.on_key(key('n'));
        a.result_views.clear();
        a.on_db_event(DbEvent::PageDone {
            rows: vec![],
            has_more: false,
            elapsed: std::time::Duration::ZERO,
        });
        assert!(a.busy.is_none());
        assert!(a.status.0.contains("discarded"), "got: {}", a.status.0);
    }

    #[test]
    fn results_without_source_are_not_pageable() {
        let mut a = app();
        let mut r = page_result(1, "SHOW TABLES", true);
        r.source = None;
        a.on_db_event(DbEvent::QueryDone {
            results: vec![r],
            message: "1 row".into(),
        });
        a.on_key(key('n'));
        assert!(a.busy.is_none());
        assert_eq!(a.status.0, "this result set cannot be paged");
    }
}
