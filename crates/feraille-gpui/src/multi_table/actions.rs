use gpui::actions;

actions!(
    feraille_multi_table,
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
