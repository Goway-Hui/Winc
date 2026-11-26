//! 语言服务器智能交互：补全、定义、引用与悬停。
//!
//! 请求用文档、revision 和启动代次共同标识；任何一项变化都丢弃迟到结果。

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use serde_json::Value;
use winc_core::Lines;
use winc_editor::DocumentId;
use winc_lsp::{Location, LocationLink, PendingRequest, Position};

use crate::app::editor::definition::{Place, PlacesKind};
use crate::app::preview::markdown::suggest;
use crate::app::{App, Focus, Hover};

const REQUEST_TIMEOUT: Duration = Duration::from_secs(5);

/// 每窗口独立的语言智能请求状态。
#[derive(Debug, Default)]
pub(crate) struct Intelligence {
    generation: u64,
    next_ticket: u64,
    completion: Option<CompletionRequest>,
    navigation: Option<NavigationRequest>,
    hover: Option<HoverRequest>,
    symbols: Option<SymbolsRequest>,
    completions: HashMap<DocumentId, Vec<winc_lsp::CompletionItem>>,
    pub(crate) hover_text: Option<(DocumentId, u64, usize, String, String)>,
    pub(crate) completion_revision: Option<(DocumentId, u64)>,
    completion_due: Option<Instant>,
}

#[derive(Debug)]
struct CompletionRequest {
    ticket: u64,
    document: DocumentId,
    revision: u64,
    offset: usize,
    position: Position,
    pending: PendingRequest,
    started: Instant,
}

#[derive(Debug)]
struct NavigationRequest {
    ticket: u64,
    document: DocumentId,
    revision: u64,
    kind: NavigationKind,
    name: String,
    offset: usize,
    position: Position,
    pending: PendingRequest,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NavigationKind {
    Definition,
    References,
}

#[derive(Debug)]
struct HoverRequest {
    ticket: u64,
    document: DocumentId,
    revision: u64,
    offset: usize,
    position: Position,
    pending: PendingRequest,
    text: String,
}

#[derive(Debug)]
struct SymbolsRequest {
    document: DocumentId,
    revision: u64,
    pending: PendingRequest,
}

#[derive(Debug, Clone, PartialEq)]
struct CompletionTicket {
    ticket: u64,
    document: DocumentId,
    revision: u64,
    offset: usize,
    position: Position,
    uri: String,
}

#[derive(Debug, Clone, PartialEq)]
struct NavigationTicket {
    ticket: u64,
    document: DocumentId,
    revision: u64,
    kind: NavigationKind,
    name: String,
    offset: usize,
    position: Position,
    uri: String,
}

#[derive(Debug, Clone, PartialEq)]
struct HoverTicket {
    ticket: u64,
    document: DocumentId,
    revision: u64,
    offset: usize,
    position: Position,
    uri: String,
}

impl Intelligence {
    pub(crate) fn schedule_completion(&mut self) {
        self.completion_due = Some(Instant::now() + Duration::from_millis(120));
    }

    pub(crate) fn reset(&mut self) {
        self.generation = self.generation.wrapping_add(1);
        self.completion = None;
        self.navigation = None;
        self.hover = None;
        self.symbols = None;
        self.hover_text = None;
        self.completions.clear();
        self.completion_revision = None;
        self.completion_due = None;
    }

    pub(crate) fn cancel_for_edit(&mut self) {
        self.completion = None;
        self.navigation = None;
        self.hover = None;
        self.symbols = None;
        self.hover_text = None;
        self.completion_revision = None;
        self.completion_due = None;
    }

    fn ticket(&mut self) -> u64 {
        self.next_ticket = self.next_ticket.wrapping_add(1);
        self.next_ticket
    }

    fn current(&self, generation: u64) -> bool {
        self.generation == generation
    }
}

impl App {
    fn intelligence_ticket_current(&self, document: DocumentId, revision: u64) -> bool {
        self.intelligence.current(self.lsp.generation())
            && self
                .editor
                .document(document)
                .is_some_and(|doc| doc.revision == revision)
    }
}

impl App {
    /// 当前光标是否仍与补全 ticket 完全一致。
    fn completion_ticket_current(&self, ticket: &CompletionTicket) -> bool {
        ticket.ticket == self.intelligence.next_ticket
            && self.editor.active().is_some_and(|doc| {
                ticket.document == doc.id
                    && ticket.revision == doc.revision
                    && ticket.offset == doc.selection.head
            })
    }

    /// 当前补全列表对应的文档版本，接受补全前必须仍然一致。
    pub(in crate::app) const fn intelligence_completion_revision(
        &self,
    ) -> Option<(DocumentId, u64)> {
        self.intelligence.completion_revision
    }

    /// 发出版本化补全请求；结果由 [`Self::poll_lsp_completion`] 收取。
    pub(in crate::app) fn request_lsp_completion(&mut self) {
        let Some(ticket) = self.make_completion_ticket() else {
            self.intelligence.completion = None;
            return;
        };
        let Some(client) = self
            .lsp
            .client
            .as_mut()
            .filter(|_| self.lsp.status == super::lsp::Status::Ready)
        else {
            return;
        };
        let pending = match client.completion(&ticket.uri, ticket.position, REQUEST_TIMEOUT) {
            Ok(pending) => pending,
            Err(_) => return,
        };
        self.intelligence.completion = Some(CompletionRequest {
            ticket: ticket.ticket,
            document: ticket.document,
            revision: ticket.revision,
            offset: ticket.offset,
            position: ticket.position,
            pending,
            started: Instant::now(),
        });
        self.intelligence.completion_revision = Some((ticket.document, ticket.revision));
        self.preview.markdown.suggest.at =
            Some((ticket.document, ticket.offset, suggest::Kind::Language));
        self.preview.markdown.suggest.hits.clear();
        self.preview.markdown.suggest.selected = 0;
        self.preview.markdown.suggest.first = 0;
        self.hits.suggest_card = None;
        self.request_redraw();
    }

    /// 非阻塞收取补全结果，并填充现有补全列表。
    pub(in crate::app) fn poll_lsp_completion(&mut self) {
        let (value, ticket) = {
            let Some(request) = self.intelligence.completion.as_mut() else {
                return;
            };
            if request.started.elapsed() >= REQUEST_TIMEOUT {
                self.intelligence.completion = None;
                self.preview.markdown.suggest.at = None;
                self.hits.suggest_card = None;
                return;
            }
            let value = match request.pending.try_recv() {
                Ok(Some(value)) => value,
                Ok(None) => return,
                Err(_) => {
                    self.intelligence.completion = None;
                    self.preview.markdown.suggest.at = None;
                    self.hits.suggest_card = None;
                    return;
                }
            };
            let ticket = CompletionTicket {
                ticket: request.ticket,
                document: request.document,
                revision: request.revision,
                offset: request.offset,
                position: request.position,
                uri: String::new(),
            };
            (value, ticket)
        };
        self.intelligence.completion = None;
        if !self.intelligence_ticket_current(ticket.document, ticket.revision)
            || !self.completion_ticket_current(&ticket)
        {
            return;
        }
        let items = parse_completion_items(value);
        self.intelligence
            .completions
            .insert(ticket.document, items.clone());
        let hits: Vec<suggest::Hit> = items
            .into_iter()
            .take(suggest::SUGGEST_ROWS)
            .map(|item| suggest::Hit::Language {
                label: item.label.clone(),
                detail: item.detail.clone().unwrap_or_default(),
                insert: item.insert_text.clone(),
            })
            .collect();
        if hits.is_empty() {
            self.preview.markdown.suggest.at = None;
            self.hits.suggest_card = None;
            return;
        }
        self.preview.markdown.suggest.at =
            Some((ticket.document, ticket.offset, suggest::Kind::Language));
        self.preview.markdown.suggest.hits = hits;
        self.preview.markdown.suggest.selected = 0;
        self.preview.markdown.suggest.first = 0;
        self.request_redraw();
    }

    /// 请求定义；服务器不可用或结果为空时退回现有本地/项目搜索。
    pub(in crate::app) fn request_lsp_definition(&mut self) {
        let Some(ticket) = self.make_navigation_ticket(NavigationKind::Definition) else {
            return self.fallback_definition();
        };
        let Some(pending) = self.send_navigation(&ticket) else {
            return self.fallback_definition();
        };
        self.intelligence.navigation = Some(NavigationRequest {
            ticket: ticket.ticket,
            document: ticket.document,
            revision: ticket.revision,
            kind: ticket.kind,
            name: ticket.name,
            offset: ticket.offset,
            position: ticket.position,
            pending,
        });
    }

    /// 请求引用；服务器不可用或结果为空时退回现有全局搜索。
    pub(in crate::app) fn request_lsp_references(&mut self) {
        let Some(ticket) = self.make_navigation_ticket(NavigationKind::References) else {
            return self.find_references();
        };
        let Some(pending) = self.send_navigation(&ticket) else {
            return self.find_references();
        };
        self.intelligence.navigation = Some(NavigationRequest {
            ticket: ticket.ticket,
            document: ticket.document,
            revision: ticket.revision,
            kind: ticket.kind,
            name: ticket.name,
            offset: ticket.offset,
            position: ticket.position,
            pending,
        });
    }

    /// 非阻塞收取定义或引用结果，并复用位置浮窗/单处跳转。
    pub(in crate::app) fn poll_lsp_navigation(&mut self) {
        let (value, ticket) = {
            let Some(request) = self.intelligence.navigation.as_mut() else {
                return;
            };
            let value = match request.pending.try_recv() {
                Ok(Some(value)) => value,
                Ok(None) => return,
                Err(_) => {
                    let kind = request.kind;
                    let document = request.document;
                    let revision = request.revision;
                    self.intelligence.navigation = None;
                    if self.intelligence_ticket_current(document, revision) {
                        if kind == NavigationKind::Definition {
                            self.fallback_definition();
                        } else {
                            self.find_references();
                        }
                    }
                    return;
                }
            };
            let ticket = NavigationTicket {
                ticket: request.ticket,
                document: request.document,
                revision: request.revision,
                kind: request.kind,
                name: request.name.clone(),
                offset: request.offset,
                position: request.position,
                uri: String::new(),
            };
            (value, ticket)
        };
        self.intelligence.navigation = None;
        if !self.intelligence_ticket_current(ticket.document, ticket.revision) {
            return;
        }
        let places = self.locations_to_places(value, &ticket);
        if places.is_empty() {
            if ticket.kind == NavigationKind::Definition {
                self.fallback_definition();
            } else {
                self.find_references();
            }
            return;
        }
        if places.len() == 1 {
            self.open_place(&places[0], &ticket.name);
        } else {
            let kind = if ticket.kind == NavigationKind::Definition {
                PlacesKind::Definitions
            } else {
                PlacesKind::Usages
            };
            self.show_places(kind, ticket.name, places);
        }
        self.request_redraw();
    }

    /// 为一个稳定的鼠标位置请求悬停内容。
    pub(in crate::app) fn request_lsp_hover_at(
        &mut self,
        document: DocumentId,
        revision: u64,
        offset: usize,
    ) {
        let Some(doc) = self
            .editor
            .document(document)
            .filter(|doc| doc.revision == revision && doc.path.is_some())
        else {
            return;
        };
        let text = doc.text().substring(0, doc.text().end_pos());
        let Some(position) = Position::from_char_offset(&text, offset) else {
            return;
        };
        let Some(uri) = self.lsp_uri_for(document) else {
            return;
        };
        let ticket = HoverTicket {
            ticket: self.intelligence.ticket(),
            document,
            revision,
            offset,
            position,
            uri,
        };
        self.request_lsp_hover(ticket);
    }

    /// 为一个稳定的鼠标位置请求悬停内容。
    fn request_lsp_hover(&mut self, ticket: HoverTicket) {
        if !self.intelligence_ticket_current(ticket.document, ticket.revision) {
            return;
        }
        if self.intelligence.hover.as_ref().is_some_and(|request| {
            request.ticket == ticket.ticket
                && request.document == ticket.document
                && request.revision == ticket.revision
                && request.offset == ticket.offset
        }) {
            return;
        }
        let Some(client) = self
            .lsp
            .client
            .as_mut()
            .filter(|_| self.lsp.status == super::lsp::Status::Ready)
        else {
            return;
        };
        let pending = match client.hover(&ticket.uri, ticket.position, REQUEST_TIMEOUT) {
            Ok(pending) => pending,
            Err(_) => return,
        };
        let text = self
            .editor
            .document(ticket.document)
            .map(|doc| doc.text().substring(0, doc.text().end_pos()))
            .unwrap_or_default();
        self.intelligence.hover = Some(HoverRequest {
            ticket: ticket.ticket,
            document: ticket.document,
            revision: ticket.revision,
            offset: ticket.offset,
            position: ticket.position,
            pending,
            text,
        });
    }

    /// 非阻塞收取悬停结果，供现有 tooltip 绘制。
    pub(in crate::app) fn poll_lsp_hover(&mut self) {
        let (value, ticket, text) = {
            let Some(request) = self.intelligence.hover.as_mut() else {
                return;
            };
            let value = match request.pending.try_recv() {
                Ok(Some(value)) => value,
                Ok(None) => return,
                Err(_) => {
                    self.intelligence.hover = None;
                    return;
                }
            };
            let ticket = HoverTicket {
                ticket: request.ticket,
                document: request.document,
                revision: request.revision,
                offset: request.offset,
                position: request.position,
                uri: String::new(),
            };
            let text = request.text.clone();
            (value, ticket, text)
        };
        self.intelligence.hover = None;
        if !self.intelligence_ticket_current(ticket.document, ticket.revision) {
            return;
        }
        if !matches!(self.pointer.hover, Hover::LanguageHover { document, revision, offset } if document == ticket.document && revision == ticket.revision && offset == ticket.offset)
        {
            return;
        }
        let Some(content) = hover_text(value) else {
            return;
        };
        self.intelligence.hover_text = Some((
            ticket.document,
            ticket.revision,
            ticket.offset,
            content,
            text,
        ));
        self.overlays.tooltip.shown = true;
        self.request_redraw();
    }

    /// 非阻塞驱动所有语言请求；仍有请求时返回 `true`，调用方应很快再次唤醒。
    pub(in crate::app) fn poll_language_requests(&mut self, now: Instant) -> bool {
        self.poll_lsp_completion();
        self.poll_lsp_navigation();
        self.poll_lsp_hover();
        self.poll_lsp_symbols();
        if let Some(due) = self.intelligence.completion_due {
            if now >= due {
                self.intelligence.completion_due = None;
                self.request_lsp_completion();
            } else {
                return true;
            }
        }
        self.intelligence.completion.is_some()
            || self.intelligence.navigation.is_some()
            || self.intelligence.hover.is_some()
            || self.intelligence.symbols.is_some()
            || self.intelligence.completion_due.is_some()
    }

    /// 请求文档符号，结果仍由现有符号列表呈现。
    pub(in crate::app) fn request_lsp_symbols(&mut self) {
        let Some(doc) = self
            .editor
            .active()
            .filter(|doc| doc.highlighter.language().id == "cangjie")
        else {
            return;
        };
        if self.lsp.status != super::lsp::Status::Ready {
            return;
        }
        let Some(uri) = self.lsp_uri_for(doc.id) else {
            return;
        };
        let pending = {
            let Some(client) = self.lsp.client.as_mut() else {
                return;
            };
            client.request(
                "textDocument/documentSymbol",
                Some(serde_json::json!({"textDocument": {"uri": uri}})),
                REQUEST_TIMEOUT,
            )
        };
        let Ok(pending) = pending else { return };
        self.intelligence.symbols = Some(SymbolsRequest {
            document: doc.id,
            revision: doc.revision,
            pending,
        });
    }

    /// 非阻塞收取文档符号；第一版转换为现有 Symbol 结构。
    pub(in crate::app) fn poll_lsp_symbols(&mut self) {
        let Some(request) = self.intelligence.symbols.as_mut() else {
            return;
        };
        let value = match request.pending.try_recv() {
            Ok(Some(value)) => value,
            Ok(None) => return,
            Err(_) => {
                self.intelligence.symbols = None;
                return;
            }
        };
        let document = request.document;
        let revision = request.revision;
        self.intelligence.symbols = None;
        if !self.intelligence_ticket_current(document, revision) {
            return;
        }
        let symbols = parse_document_symbols(value);
        self.open_lsp_symbols(document, symbols);
    }
}

impl App {
    fn make_completion_ticket(&mut self) -> Option<CompletionTicket> {
        let doc = self.editor.active()?;
        if doc.highlighter.language().id != "cangjie"
            || !doc.shows_text()
            || doc.is_pending()
            || doc.is_large()
        {
            return None;
        }
        let uri = self.lsp_uri_for(doc.id)?;
        let offset = doc.selection.head;
        if !doc.selection.is_empty() || !doc.extra_selections().is_empty() {
            return None;
        }
        let text = doc.text().substring(0, doc.text().end_pos());
        let position = Position::from_char_offset(&text, offset)?;
        let ticket = self.intelligence.ticket();
        Some(CompletionTicket {
            ticket,
            document: doc.id,
            revision: doc.revision,
            offset,
            position,
            uri,
        })
    }

    fn make_navigation_ticket(&mut self, kind: NavigationKind) -> Option<NavigationTicket> {
        let doc = self.editor.active()?;
        if doc.highlighter.language().id != "cangjie"
            || !doc.shows_text()
            || doc.is_pending()
            || doc.is_large()
        {
            return None;
        }
        let uri = self.lsp_uri_for(doc.id)?;
        let (start, end) = self.name_span()?;
        let name = doc.text().substring(start, end);
        if name.is_empty() {
            return None;
        }
        let text = doc.text().substring(0, doc.text().end_pos());
        let position = Position::from_char_offset(&text, start)?;
        let ticket = self.intelligence.ticket();
        Some(NavigationTicket {
            ticket,
            document: doc.id,
            revision: doc.revision,
            kind,
            name,
            offset: start,
            position,
            uri,
        })
    }

    fn send_navigation(&mut self, ticket: &NavigationTicket) -> Option<PendingRequest> {
        let client = self.lsp.client.as_mut()?;
        if self.lsp.status != super::lsp::Status::Ready {
            return None;
        }
        match ticket.kind {
            NavigationKind::Definition => client
                .definition(&ticket.uri, ticket.position, REQUEST_TIMEOUT)
                .ok(),
            NavigationKind::References => client
                .references(&ticket.uri, ticket.position, true, REQUEST_TIMEOUT)
                .ok(),
        }
    }

    fn lsp_uri_for(&self, id: DocumentId) -> Option<String> {
        self.editor
            .document(id)?
            .path
            .as_ref()
            .map(|path| super::super::lsp::file_uri(path))
    }

    fn fallback_definition(&mut self) {
        self.local_go_to_definition();
    }

    fn locations_to_places(&self, value: Value, ticket: &NavigationTicket) -> Vec<Place> {
        let mut places = Vec::new();
        if let Ok(location) = serde_json::from_value::<Location>(value.clone()) {
            push_lsp_location(&mut places, location, ticket, self);
        } else if let Ok(links) = serde_json::from_value::<Vec<LocationLink>>(value.clone()) {
            for link in links {
                if let Some(path) = uri_to_path(&link.target_uri) {
                    places.push(self.lsp_place(
                        path,
                        link.target_selection_range,
                        link.target_range,
                        ticket,
                    ));
                }
            }
        } else if let Ok(locations) = serde_json::from_value::<Vec<Location>>(value) {
            for location in locations {
                push_lsp_location(&mut places, location, ticket, self);
            }
        }
        places.sort_by(|a, b| {
            a.path
                .cmp(&b.path)
                .then(a.line.cmp(&b.line))
                .then(a.col.cmp(&b.col))
        });
        places.dedup_by(|a, b| a.path == b.path && a.line == b.line && a.col == b.col);
        places
    }
}

fn push_lsp_location(
    places: &mut Vec<Place>,
    location: Location,
    ticket: &NavigationTicket,
    app: &App,
) {
    if let Some(path) = uri_to_path(&location.uri) {
        places.push(app.lsp_place(path, location.range, location.range, ticket));
    }
}

impl App {
    fn lsp_place(
        &self,
        path: PathBuf,
        selection: winc_lsp::Range,
        _target: winc_lsp::Range,
        _ticket: &NavigationTicket,
    ) -> Place {
        let document = self
            .editor
            .documents
            .iter()
            .find(|doc| doc.path.as_deref() == Some(path.as_path()));
        let (line, col, preview, marks) = document.map_or_else(
            || {
                (
                    selection.start.line as usize,
                    selection.start.character as usize,
                    String::new(),
                    Vec::new(),
                )
            },
            |doc| {
                let text = doc.text().substring(0, doc.text().end_pos());
                let start = selection.start.to_char_offset(&text).unwrap_or(0);
                let end = selection.end.to_char_offset(&text).unwrap_or(start);
                let start_byte = text
                    .char_indices()
                    .nth(start)
                    .map(|(byte, _)| byte)
                    .unwrap_or(text.len());
                let line_start = text[..start_byte].rfind('\n').map_or(0, |byte| byte + 1);
                let line_start_char = text[..line_start].chars().count();
                let line_end = text[line_start..]
                    .find(['\r', '\n'])
                    .map_or(text.len(), |byte| line_start + byte);
                (
                    selection.start.line as usize,
                    start.saturating_sub(line_start_char),
                    text[line_start..line_end].trim_start().to_owned(),
                    std::iter::once(
                        start.saturating_sub(line_start_char)..end.saturating_sub(line_start_char),
                    )
                    .collect(),
                )
            },
        );
        let relative = self
            .editor
            .workspace
            .as_ref()
            .map(|workspace| workspace.labelled_path(&path))
            .unwrap_or_else(|| {
                path.file_name()
                    .and_then(|name| name.to_str())
                    .unwrap_or_default()
                    .to_owned()
            });
        Place {
            path,
            relative,
            line,
            col: Some(col),
            preview,
            marks,
        }
    }
}

fn uri_to_path(uri: &str) -> Option<PathBuf> {
    let rest = uri.strip_prefix("file://")?;
    let (path, _fragment) = rest.split_once('#').unwrap_or((rest, ""));
    if path.is_empty() {
        return None;
    }
    let bytes = percent_decode(path.as_bytes())?;
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStringExt;
        Some(PathBuf::from(std::ffi::OsString::from_vec(bytes)))
    }
    #[cfg(windows)]
    {
        let path = String::from_utf8(bytes).ok()?;
        Some(PathBuf::from(
            path.strip_prefix('/').unwrap_or(&path).replace('/', "\\"),
        ))
    }
    #[cfg(not(any(unix, windows)))]
    None
}

fn percent_decode(bytes: &[u8]) -> Option<Vec<u8>> {
    let mut output = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            let high = *bytes.get(index + 1)?;
            let low = *bytes.get(index + 2)?;
            let value = (hex(high)? << 4) | hex(low)?;
            output.push(value);
            index += 3;
        } else {
            output.push(bytes[index]);
            index += 1;
        }
    }
    Some(output)
}

fn hex(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

fn hover_text(value: Value) -> Option<String> {
    if value.is_null() {
        return None;
    }
    if let Some(text) = value.as_str() {
        return Some(text.to_owned());
    }
    let contents = value.get("contents").unwrap_or(&value);
    if let Some(text) = contents.as_str() {
        return Some(text.to_owned());
    }
    if let Some(text) = contents.get("value").and_then(Value::as_str) {
        return Some(text.to_owned());
    }
    if let Some(items) = contents.as_array() {
        let text: Vec<&str> = items
            .iter()
            .filter_map(|item| {
                item.get("value")
                    .and_then(Value::as_str)
                    .or_else(|| item.as_str())
            })
            .collect();
        if !text.is_empty() {
            return Some(text.join("\n"));
        }
    }
    None
}

fn parse_completion_items(value: Value) -> Vec<winc_lsp::CompletionItem> {
    if value.is_array() {
        serde_json::from_value(value).unwrap_or_default()
    } else if let Some(items) = value.get("items") {
        serde_json::from_value(items.clone()).unwrap_or_default()
    } else {
        Vec::new()
    }
}

fn parse_document_symbols(value: Value) -> Vec<winc_editor::symbols::Symbol> {
    fn walk(
        items: &[Value],
        output: &mut Vec<winc_editor::symbols::Symbol>,
        container: Option<&str>,
        depth: usize,
    ) {
        for item in items {
            let Some(name) = item.get("name").and_then(Value::as_str) else {
                continue;
            };
            let kind = match item.get("kind").and_then(Value::as_u64).unwrap_or(0) {
                5 => winc_editor::symbols::SymbolKind::Type,
                6 => winc_editor::symbols::SymbolKind::Function,
                9 => winc_editor::symbols::SymbolKind::Type,
                10 => winc_editor::symbols::SymbolKind::Module,
                12 | 13 => winc_editor::symbols::SymbolKind::Variable,
                _ => winc_editor::symbols::SymbolKind::Function,
            };
            let range = item.get("selectionRange").or_else(|| item.get("range"));
            let line = range
                .and_then(|range| range.get("start"))
                .and_then(|start| start.get("line"))
                .and_then(Value::as_u64)
                .unwrap_or(0) as usize;
            let col = range
                .and_then(|range| range.get("start"))
                .and_then(|start| start.get("character"))
                .and_then(Value::as_u64)
                .unwrap_or(0) as usize;
            output.push(winc_editor::symbols::Symbol {
                name: name.to_owned(),
                kind,
                line,
                col,
                container: container.map(str::to_owned),
                depth,
                heading: 0,
            });
            if let Some(children) = item.get("children").and_then(Value::as_array) {
                walk(children, output, Some(name), depth + 1);
            }
        }
    }
    let mut output = Vec::new();
    if let Some(items) = value.as_array() {
        walk(items, &mut output, None, 0);
    }
    output
}

impl App {
    fn open_lsp_symbols(&mut self, document: DocumentId, items: Vec<winc_editor::symbols::Symbol>) {
        if self.editor.active().map(|doc| doc.id) != Some(document) || items.is_empty() {
            return;
        }
        let scroll = self
            .editor
            .active()
            .map(|doc| (doc.scroll_line, doc.scroll_x))
            .unwrap_or_default();
        self.overlays.picker.symbols = Some(super::symbols::SymbolList {
            id: document,
            items,
            too_large: false,
            scroll,
        });
        self.overlays.picker.input.set_text("@");
        self.overlays.picker.open = true;
        self.focus = Focus::Picker;
        self.picker_refresh();
        self.relayout();
        self.request_redraw();
    }
}
