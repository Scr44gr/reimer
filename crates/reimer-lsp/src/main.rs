use tokio::sync::RwLock;
use tower_lsp::jsonrpc::Result;
use tower_lsp::lsp_types::{
    CodeActionOptions, CodeActionParams, CodeActionProviderCapability, CodeLensOptions,
    CompletionOptions, CompletionParams, CompletionResponse, DidChangeTextDocumentParams,
    DidChangeWatchedFilesParams, DidCloseTextDocumentParams, DidOpenTextDocumentParams,
    DidSaveTextDocumentParams, DocumentSymbolParams, DocumentSymbolResponse, GotoDefinitionParams,
    GotoDefinitionResponse, HoverParams, HoverProviderCapability, InitializeParams,
    InitializeResult, InitializedParams, InlayHintOptions, InlayHintParams,
    InlayHintServerCapabilities, MessageType, OneOf, PrepareRenameResponse, RenameOptions,
    RenameParams, ServerCapabilities, ServerInfo, TextDocumentPositionParams,
    TextDocumentSyncCapability, TextDocumentSyncKind, TextDocumentSyncOptions, Url,
    WorkDoneProgressOptions, WorkspaceEdit,
};
use tower_lsp::{Client, LanguageServer, LspService, Server};

use reimer_lsp::Workspace;

struct Backend {
    client: Client,
    workspace: RwLock<Workspace>,
}

impl Backend {
    fn new(client: Client) -> Self {
        Self {
            client,
            workspace: RwLock::new(Workspace::default()),
        }
    }

    async fn publish(&self, uri: Url, text: String, version: Option<i32>) {
        let affected = self.workspace.write().await.update(uri.clone(), text);
        self.publish_updates(affected, Some((uri, version))).await;
    }

    async fn publish_updates(&self, affected: Vec<Url>, current: Option<(Url, Option<i32>)>) {
        let updates = {
            let workspace = self.workspace.read().await;
            affected
                .into_iter()
                .filter_map(|uri| {
                    workspace
                        .get(&uri)
                        .map(|document| (uri, document.diagnostics()))
                })
                .collect::<Vec<_>>()
        };
        for (uri, diagnostics) in updates {
            let version = current
                .as_ref()
                .filter(|(current_uri, _)| current_uri == &uri)
                .and_then(|(_, version)| *version);
            self.client
                .publish_diagnostics(uri, diagnostics, version)
                .await;
        }
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
                rename_provider: Some(OneOf::Right(RenameOptions {
                    prepare_provider: Some(true),
                    work_done_progress_options: WorkDoneProgressOptions {
                        work_done_progress: None,
                    },
                })),
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
            self.workspace
                .read()
                .await
                .get(&params.text_document.uri)
                .map(|document| document.text().to_owned())
        };
        if let Some(text) = text {
            self.publish(params.text_document.uri, text, None).await;
        }
    }

    async fn did_change_watched_files(&self, params: DidChangeWatchedFilesParams) {
        let paths = params
            .changes
            .into_iter()
            .filter_map(|change| change.uri.to_file_path().ok())
            .collect::<Vec<_>>();
        let affected = self.workspace.write().await.refresh_paths(&paths);
        self.publish_updates(affected, None).await;
    }

    async fn did_close(&self, params: DidCloseTextDocumentParams) {
        let uri = params.text_document.uri;
        let affected = self.workspace.write().await.close(&uri);
        self.client.publish_diagnostics(uri, Vec::new(), None).await;
        self.publish_updates(affected, None).await;
    }

    async fn hover(&self, params: HoverParams) -> Result<Option<tower_lsp::lsp_types::Hover>> {
        let text_document = params.text_document_position_params;
        Ok(self
            .workspace
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
            .workspace
            .read()
            .await
            .get(&text_document.text_document.uri)
            .and_then(|document| document.definition(text_document.position))
            .map(GotoDefinitionResponse::Scalar))
    }

    async fn prepare_rename(
        &self,
        params: TextDocumentPositionParams,
    ) -> Result<Option<PrepareRenameResponse>> {
        Ok(self
            .workspace
            .read()
            .await
            .get(&params.text_document.uri)
            .and_then(|document| document.prepare_rename(params.position)))
    }

    async fn rename(&self, params: RenameParams) -> Result<Option<WorkspaceEdit>> {
        let text_document = params.text_document_position;
        Ok(self
            .workspace
            .read()
            .await
            .get(&text_document.text_document.uri)
            .and_then(|document| document.rename(text_document.position, &params.new_name)))
    }

    async fn document_symbol(
        &self,
        params: DocumentSymbolParams,
    ) -> Result<Option<DocumentSymbolResponse>> {
        Ok(self
            .workspace
            .read()
            .await
            .get(&params.text_document.uri)
            .map(|document| DocumentSymbolResponse::Nested(document.document_symbols())))
    }

    async fn completion(&self, params: CompletionParams) -> Result<Option<CompletionResponse>> {
        Ok(self
            .workspace
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
            .workspace
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
            .workspace
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
            .workspace
            .read()
            .await
            .get(&params.text_document.uri)
            .map(reimer_lsp::Document::code_lenses))
    }
}

#[tokio::main]
async fn main() {
    let stdin = tokio::io::stdin();
    let stdout = tokio::io::stdout();
    let (service, socket) = LspService::new(Backend::new);
    Server::new(stdin, stdout, socket).serve(service).await;
}
