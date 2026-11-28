function(value) {
    this.focus();
    // A contenteditable element has no value to set: select its contents, and the caller
    // replaces them through the keyboard, which fires the page's own input events.
    if (this.isContentEditable) {
        var range = document.createRange();
        range.selectNodeContents(this);
        var selection = window.getSelection();
        selection.removeAllRanges();
        selection.addRange(range);
        return 'editable';
    }
    this.value = value;
    this.dispatchEvent(new Event('input', { bubbles: true }));
    this.dispatchEvent(new Event('change', { bubbles: true }));
    return 'value';
}
