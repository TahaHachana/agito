function() {
    if (this.contentEditable === 'true') return true;
    var role = this.getAttribute('role');
    if (role === 'textbox') return true;
    var tag = this.tagName.toLowerCase();
    if (tag === 'input' || tag === 'textarea') return !this.disabled && !this.readOnly;
    return false;
}
