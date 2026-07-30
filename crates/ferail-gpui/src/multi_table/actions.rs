use gpui::actions;

actions!(
    ferail_multi_table,
    [
        Cancel,
        SelectUp,
        SelectDown,
        SelectFirst,
        SelectLast,
        SelectPrevColumn,
        SelectNextColumn,
        SelectPageUp,
        SelectPageDown
    ]
);
