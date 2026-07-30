use std::collections::HashMap;

use tokio::sync::RwLock;
use tower_lsp::jsonrpc::Result;
use tower_lsp::lsp_types::{
    CodeActionOptions, CodeActionParams, CodeActionProviderCapability, CodeLensOptions,
    CompletionOptions, CompletionParams, CompletionResponse, DidChangeTextDocumentParams,
    DidChangeWatchedFilesParams, DidCloseTextDocumentParams, DidOpenTextDocumentParams,
    DidSaveTextDocumentParams, DocumentSymbolParams, DocumentSymbolResponse, GotoDefinitionParams,
    GotoDefinitionResponse, HoverParams, HoverProviderCapability, InitializeParams,
    InitializeResult, InitializedParams, InlayHintOptions, InlayHintParams,
    InlayHintServerCapabilities, MessageType, OneOf, ServerCapabilities, ServerInfo,
    TextDocumentSyncCapability, TextDocumentSyncKind, TextDocumentSyncOptions, Url,
    WorkDoneProgressOptions,
};
use tower_lsp::{Client, LanguageServer, LspService, Server};

use reimer_lsp::Document;

struct Backend {
    client: Client,
    documents: RwLock<HashMap<Url, Document>>,
}

impl Backend {
    fn new(client: Client) -> Self {
        Self {
            client,
            documents: RwLock::new(HashMap::new()),
        }
    }

    async fn publish(&self, uri: Url, text: String, version: Option<i32>) {
        let document = Document::new(uri.clone(), text);
        let diagnostics = document.diagnostics();
        self.documents.write().await.insert(uri.clone(), document);
        self.client
            .publish_diagnostics(uri, diagnostics, version)
            .await;
    }
}

#[tower_lsp::async_trait]
impl LanguageServer for Backend {
    async fn initialize(&self, _: InitializeParams) -> Result<InitializeResult> {
        Ok(InitializeResult {
            capabilities: ServerCapabilities {
                text_document_sync: Some(TextDocumentSyncCapability::Options(
                    TextDocumentSyncOptions {
                        open_close: Some(true),
                        change: Some(TextDocumentSyncKind::FULL),
                        will_save: None,
                        will_save_wait_until: None,
                        save: Some(
                            tower_lsp::lsp_types::TextDocumentSyncSaveOptions::Supported(true),
                        ),
                    },
                )),
                hover_provider: Some(HoverProviderCapability::Simple(true)),
                definition_provider: Some(OneOf::Left(true)),
                document_symbol_provider: Some(OneOf::Left(true)),
                completion_provider: Some(CompletionOptions {
                    resolve_provider: Some(false),
                    trigger_characters: Some(vec![".".to_owned(), ":".to_owned()]),
                    all_commit_characters: None,
                    work_done_progress_options: WorkDoneProgressOptions {
                        work_done_progress: None,
                    },
                    completion_item: None,
                }),
                code_action_provider: Some(CodeActionProviderCapability::Options(
                    CodeActionOptions {
                        code_action_kinds: Some(vec![
                            tower_lsp::lsp_types::CodeActionKind::QUICKFIX,
                            tower_lsp::lsp_types::CodeActionKind::SOURCE_ORGANIZE_IMPORTS,
                        ]),
                        work_done_progress_options: WorkDoneProgressOptions {
                            work_done_progress: None,
                        },
                        resolve_provider: Some(false),
                    },
                )),
                inlay_hint_provider: Some(OneOf::Right(InlayHintServerCapabilities::Options(
                    InlayHintOptions {
                        work_done_progress_options: WorkDoneProgressOptions {
                            work_done_progress: None,
                        },
                        resolve_provider: Some(false),
                    },
                ))),
                code_lens_provider: Some(CodeLensOptions {
                    resolve_provider: Some(false),
                }),
                ..ServerCapabilities::default()
            },
            server_info: Some(ServerInfo {
                name: "Reimer Language Server".to_owned(),
                version: Some(env!("CARGO_PKG_VERSION").to_owned()),
            }),
        })
    }

    async fn initialized(&self, _: InitializedParams) {
        self.client
            .log_message(
                MessageType::INFO,
                "Language server initialized with compiler-backed diagnostics",
            )
            .await;
    }

    async fn shutdown(&self) -> Result<()> {
        Ok(())
    }

    async fn did_open(&self, params: DidOpenTextDocumentParams) {
        self.publish(
            params.text_document.uri,
            params.text_document.text,
            Some(params.text_document.version),
        )
        .await;
    }

    async fn did_change(&self, params: DidChangeTextDocumentParams) {
        let Some(change) = params.content_changes.into_iter().last() else {
            return;
        };
        self.publish(
            params.text_document.uri,
            change.text,
            Some(params.text_document.version),
        )
        .await;
    }

    async fn did_save(&self, params: DidSaveTextDocumentParams) {
        let text = if let Some(text) = params.text {
            Some(text)
        } else {
            self.documents
                .read()
                .await
                .get(&params.text_document.uri)
                .map(|document| document.text().to_owned())
        };
        if let Some(text) = text {
            self.publish(params.text_document.uri, text, None).await;
        }
    }

    async fn did_change_watched_files(&self, _: DidChangeWatchedFilesParams) {
        let snapshots = self
            .documents
            .read()
            .await
            .iter()
            .map(|(uri, document)| (uri.clone(), document.text().to_owned()))
            .collect::<Vec<_>>();
        for (uri, text) in snapshots {
            self.publish(uri, text, None).await;
        }
    }

    async fn did_close(&self, params: DidCloseTextDocumentParams) {
        self.documents
            .write()
            .await
            .remove(&params.text_document.uri);
        self.client
            .publish_diagnostics(params.text_document.uri, Vec::new(), None)
            .await;
    }

    async fn hover(&self, params: HoverParams) -> Result<Option<tower_lsp::lsp_types::Hover>> {
        let text_document = params.text_document_position_params;
        Ok(self
            .documents
            .read()
            .await
            .get(&text_document.text_document.uri)
            .and_then(|document| document.hover(text_document.position)))
    }

    async fn goto_definition(
        &self,
        params: GotoDefinitionParams,
    ) -> Result<Option<GotoDefinitionResponse>> {
        let text_document = params.text_document_position_params;
        Ok(self
            .documents
            .read()
            .await
            .get(&text_document.text_document.uri)
            .and_then(|document| document.definition(text_document.position))
            .map(GotoDefinitionResponse::Scalar))
    }

    async fn document_symbol(
        &self,
        params: DocumentSymbolParams,
    ) -> Result<Option<DocumentSymbolResponse>> {
        Ok(self
            .documents
            .read()
            .await
            .get(&params.text_document.uri)
            .map(|document| DocumentSymbolResponse::Nested(document.document_symbols())))
    }

    async fn completion(&self, params: CompletionParams) -> Result<Option<CompletionResponse>> {
        Ok(self
            .documents
            .read()
            .await
            .get(&params.text_document_position.text_document.uri)
            .map(|document| CompletionResponse::Array(document.completions())))
    }

    async fn code_action(
        &self,
        params: CodeActionParams,
    ) -> Result<Option<tower_lsp::lsp_types::CodeActionResponse>> {
        Ok(self
            .documents
            .read()
            .await
            .get(&params.text_document.uri)
            .map(|document| document.code_actions(params.range)))
    }

    async fn inlay_hint(
        &self,
        params: InlayHintParams,
    ) -> Result<Option<Vec<tower_lsp::lsp_types::InlayHint>>> {
        Ok(self
            .documents
            .read()
            .await
            .get(&params.text_document.uri)
            .map(|document| document.inlay_hints(params.range)))
    }

    async fn code_lens(
        &self,
        params: tower_lsp::lsp_types::CodeLensParams,
    ) -> Result<Option<Vec<tower_lsp::lsp_types::CodeLens>>> {
        Ok(self
            .documents
            .read()
            .await
            .get(&params.text_document.uri)
            .map(Document::code_lenses))
    }
}

#[tokio::main]
async fn main() {
    let stdin = tokio::io::stdin();
    let stdout = tokio::io::stdout();
    let (service, socket) = LspService::new(Backend::new);
    Server::new(stdin, stdout, socket).serve(service).await;
}
