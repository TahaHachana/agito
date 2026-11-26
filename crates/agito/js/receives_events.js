function() {
    var r = this.getBoundingClientRect();
    var cx = r.x + r.width / 2;
    var cy = r.y + r.height / 2;
    // Hit-test in the element's own tree: from the document, a shadow child is hidden
    // behind its host.
    var root = this.getRootNode();
    var top = (root.elementFromPoint ? root : document).elementFromPoint(cx, cy);
    if (top === null) return false;
    if (this === top || this.contains(top)) return true;
    var style = window.getComputedStyle(top);
    if (style.pointerEvents === 'none') return true;
    var el = top;
    while (el) {
        if (el === this) return true;
        el = el.parentElement;
    }
    var targetParent = this.parentElement;
    if (targetParent) {
        el = top;
        while (el) {
            if (el === targetParent) return true;
            el = el.parentElement;
        }
    }
    return false;
}
