mod clear_button;
mod content_type;
mod input;
mod token;
pub use gpui_base::input::{
    ActivateToken, InlineToken, InlineTokenClickEvent, InlineTokenContext, InlineTokenError,
    InlineTokenHoverEvent, InlineTokenSpan, InputContent,
};
pub use token::InputToken;
pub mod language_config;
mod number_input;
mod otp_input;
mod overlay;
pub(crate) mod popovers;
mod search;
pub(crate) mod syntax_context;

pub(crate) use clear_button::*;
pub use content_type::*;
#[cfg(not(feature = "tree-sitter"))]
pub struct Tree;
/// The shared editing engine. Internal to the framework: components reach it
/// through the concrete state of their control, never across the public API.
pub(crate) use gpui_base::input::InputBaseState;
pub use gpui_base::input::{
    Backspace, BufferPoint, CodeActionItem, CodeActionProvider, CompletionHighlighter,
    CompletionMenuOptions, CompletionProvider, Copy, Cut, DefinitionProvider, Delete,
    DeleteToBeginningOfLine, DeleteToEndOfLine, DeleteToNextWordEnd, DeleteToPreviousWordStart,
    DisplayMap, DisplayPoint, DocumentColorProvider, DocumentRangeSemanticTokensProvider,
    EditorState, Enter, Escape, FoldRange, GoToDefinition, HighlightStyleResolver,
    HoverPopoverState, HoverProvider, Indent, IndentInline, InputEdit, InputEvent,
    InputHighlighter, InputHighlighterFactory, InputState, Lsp, MaskPattern, MoveDown, MoveEnd,
    MoveHome, MoveLeft, MovePageDown, MovePageUp, MoveRight, MoveToEnd, MoveToEndOfLine,
    MoveToNextSubwordEnd, MoveToNextWord, MoveToPreviousSubwordStart, MoveToPreviousWord,
    MoveToStart, MoveToStartOfLine, MoveUp, Outdent,
    OutdentInline, Paste, Point, PrepaintCaretGeometry, RangeDecoration, RangeDecorationCollection,
    RangeDecorationStyle, Redo, Replace, Rope, RopeExt, RopeLines, Search, SelectAll, SelectToEnd,
    SelectToEndOfLine, SelectToNextSubwordEnd, SelectToNextWordEnd, SelectToPreviousSubwordStart,
    SelectToPreviousWordStart, SelectToStart,
    SelectToStartOfLine, Selection, ShowCharacterPalette, ShowCompletions, ShowDocumentHandler,
    TabSize, TextDecoration, TextDecorationCollection, TextareaState, ToggleCodeActions, Undo,
    WrappingIndent, highlight_characters, highlight_contains, highlight_prefix,
};
pub use gpui_base::input::{DuplicateLine, MoveLineDown, MoveLineUp};
pub use gpui_base::input::{EditorMode, InputMode, InputModeKind, TextareaMode};
pub use gpui_base::input::ToggleComments;
pub use gpui_base::input::{SelectLargerSyntaxNode, SelectSmallerSyntaxNode};
#[doc(hidden)]
mod editor;
mod group;
mod state;
mod textarea;
pub use editor::Editor;
pub use gpui_base::input::{
    AutoClosingPair, BracketPair, IndentationRules, LanguageProvider, SyntaxContext,
    SyntaxContextProvider, set_language_config, set_language_provider,
};
pub use group::*;
pub use input::*;
pub use lsp_types::Position;
// The types a `CompletionProvider` is written in.
pub use lsp_types::{
    Command, CompletionContext, CompletionItem, CompletionItemKind, CompletionResponse,
    CompletionTextEdit, Range as LspRange, TextEdit,
};
pub use number_input::{NumberInput, NumberInputEvent, NumberStep, StepAction};
pub use otp_input::*;
pub use state::AnyInputState;
pub use textarea::Textarea;

pub(crate) fn init(cx: &mut gpui::App) {
    language_config::init(cx);
}
