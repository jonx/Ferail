use std::ops::Range;

use gpui::{
    App, Context, Div, InteractiveElement as _, IntoElement, ParentElement as _, Pixels,
    SharedString, Stateful, Styled as _, Window, div,
};

use super::{Column, ColumnGroup, ColumnSort, TableState, loading::Loading};
use gpui_component::{ActiveTheme as _, Icon, IconName, Size, h_flex, menu::PopupMenu};

/// A delegate trait for providing data and rendering for a table.
#[allow(unused)]
pub trait TableDelegate: Sized + 'static {
    /// Return the number of columns in the table.
    fn columns_count(&self, cx: &App) -> usize;

    /// Return the number of rows in the table.
    fn rows_count(&self, cx: &App) -> usize;

    /// Returns the table column at the given index.
    ///
    /// This only call on Table prepare or refresh.
    fn column(&self, col_ix: usize, cx: &App) -> Column;

    /// The text shown for the column at `col_ix`: in the header cell,
    /// the drag ghost, the column picker and the autofit measurement.
    /// Defaults to [`Column::name`]; a delegate whose headers are
    /// localized overrides this so the label is looked up at render time
    /// (a language switch repaints without rebuilding the columns).
    fn column_name(&self, col_ix: usize, cx: &App) -> SharedString {
        self.column(col_ix, cx).name.clone()
    }

    /// Perform sort on the column at the given index.
    fn perform_sort(
        &mut self,
        col_ix: usize,
        sort: ColumnSort,
        window: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) {
    }

    /// Render the table head row.
    fn render_header(
        &mut self,
        window: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) -> Stateful<Div> {
        div().id("header")
    }

    /// Return the group headers definitions (can be multi-level).
    ///
    /// By default, it returns None, meaning no group headers.
    fn group_headers(&self, cx: &App) -> Option<Vec<Vec<ColumnGroup>>> {
        None
    }

    /// Custom render for a group header cell.
    /// Receives the group label, the logical col_span, and the pixel width.
    fn render_group_th(
        &mut self,
        label: &SharedString,
        _col_span: usize,
        width: Pixels,
        _window: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) -> impl IntoElement {
        div()
            .w(width)
            .h_full()
            .flex_shrink_0()
            .flex()
            .items_center()
            .justify_center()
            .border_r_1()
            .border_color(cx.theme().border)
            .child(label.clone())
    }

    /// Render the header cell at the given column index, default to the column name.
    fn render_th(
        &mut self,
        col_ix: usize,
        window: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) -> impl IntoElement {
        div().size_full().child(self.column_name(col_ix, cx))
    }

    /// Render the row at the given row and column.
    ///
    /// Not include the table head row.
    fn render_tr(
        &mut self,
        row_ix: usize,
        window: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) -> Stateful<Div> {
        div().id(("row", row_ix))
    }

    /// Extra hover refinement for the row at `row_ix`, merged into the
    /// table's single row `.hover()`.
    ///
    /// gpui allows exactly one hover style per element (a second `.hover()`
    /// is a debug assertion), and the table owns the row's. A delegate that
    /// wants a hover ring, e.g. a drop-target highlight while a native drag
    /// is in flight: returns it here instead of calling `.hover()` on the
    /// `render_tr` row.
    fn render_tr_hover(
        &mut self,
        _row_ix: usize,
        _window: &mut Window,
        _cx: &mut Context<TableState<Self>>,
    ) -> Option<gpui::StyleRefinement> {
        None
    }

    /// Render the context menu for the row at the given row index.
    fn context_menu(
        &mut self,
        row_ix: usize,
        menu: PopupMenu,
        window: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) -> PopupMenu {
        menu
    }

    /// Build the context menu for a right-click on the table's empty space:
    /// below the last row, or anywhere in an empty folder's body. The header
    /// and the rows have their own menus and never reach this. Default
    /// returns the menu unchanged; an empty menu is suppressed, so a
    /// delegate that doesn't implement this keeps the empty space inert.
    ///
    /// `TableEvent::RightClickedBackground` is emitted just before this
    /// runs, so a subscriber can stage the menu's target (the file list's
    /// Shell stages the current directory).
    fn background_context_menu(
        &mut self,
        menu: PopupMenu,
        window: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) -> PopupMenu {
        let _ = (window, cx);
        menu
    }

    /// Whether this delegate wants a context menu on the column header
    /// (right-click). Default `false`: the header is inert.
    fn header_has_menu(&self, cx: &App) -> bool {
        let _ = cx;
        false
    }

    /// Build the column-header context menu (right-click). Only consulted
    /// when [`Self::header_has_menu`] is `true`. Default returns the menu
    /// unchanged.
    fn header_context_menu(
        &mut self,
        menu: PopupMenu,
        window: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) -> PopupMenu {
        let _ = (window, cx);
        menu
    }

    /// Render cell at the given row and column.
    fn render_td(
        &mut self,
        row_ix: usize,
        col_ix: usize,
        window: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) -> impl IntoElement;

    /// Move the column at the given `col_ix` to insert before the column at the given `to_ix`.
    fn move_column(
        &mut self,
        col_ix: usize,
        to_ix: usize,
        window: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) {
    }

    /// Return a Element to show when table is empty.
    fn render_empty(
        &mut self,
        window: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) -> impl IntoElement {
        h_flex()
            .size_full()
            .justify_center()
            .text_color(cx.theme().muted_foreground.opacity(0.6))
            .child(Icon::new(IconName::Inbox).size_12())
            .into_any_element()
    }

    /// Return true to show the loading view.
    fn loading(&self, cx: &App) -> bool {
        false
    }

    /// Return a Element to show when table is loading, default is built-in Skeleton loading view.
    ///
    /// The size is the size of the Table.
    fn render_loading(
        &mut self,
        size: Size,
        window: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) -> impl IntoElement {
        Loading::new().size(size)
    }

    /// Return true to enable load more data when scrolling to the bottom.
    ///
    /// Default: false
    fn has_more(&self, cx: &App) -> bool {
        false
    }

    /// Returns a threshold value (n rows), of course, when scrolling to the bottom,
    /// the remaining number of rows triggers `load_more`.
    /// This should smaller than the total number of first load rows.
    ///
    /// Default: 20 rows
    fn load_more_threshold(&self) -> usize {
        20
    }

    /// Load more data when the table is scrolled to the bottom.
    ///
    /// This will performed in a background task.
    ///
    /// This is always called when the table is near the bottom,
    /// so you must check if there is more data to load or lock the loading state.
    fn load_more(&mut self, window: &mut Window, cx: &mut Context<TableState<Self>>) {}

    /// Render the last empty column, default to empty.
    fn render_last_empty_col(
        &mut self,
        window: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) -> impl IntoElement {
        h_flex().w_3().h_full().flex_shrink_0()
    }

    /// Called when the visible range of the rows changed.
    ///
    /// NOTE: Make sure this method is fast, because it will be called frequently.
    ///
    /// This can used to handle some data update, to only update the visible rows.
    /// Please ensure that the data is updated in the background task.
    fn visible_rows_changed(
        &mut self,
        visible_range: Range<usize>,
        window: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) {
    }

    /// Called when the visible range of the columns changed.
    ///
    /// NOTE: Make sure this method is fast, because it will be called frequently.
    ///
    /// This can used to handle some data update, to only update the visible rows.
    /// Please ensure that the data is updated in the background task.
    fn visible_columns_changed(
        &mut self,
        visible_range: Range<usize>,
        window: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) {
    }

    /// Get the text representation of a cell for export purposes (e.g., CSV export).
    ///
    /// Returns an empty string by default. Implement this method to support export.
    /// The text should be formatted as it should appear in the exported data.
    fn cell_text(&self, row_ix: usize, col_ix: usize, cx: &App) -> String {
        String::new()
    }

    /// Extra width, beyond the measured `cell_text` plus cell padding,
    /// that double-click-to-fit (`TableState::autofit_col`) should add
    /// for this column, for leading icons / trailing badges that aren't
    /// part of the text. Defaults to 0 (pure-text columns).
    fn autofit_extra(&self, _col_ix: usize, _cx: &App) -> Pixels {
        gpui::px(0.0)
    }
}
