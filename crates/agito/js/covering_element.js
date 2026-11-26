function() {
    var r = this.getBoundingClientRect();
    var cx = r.x + r.width / 2;
    var cy = r.y + r.height / 2;
    // Hit-test in the element's own tree: from the document, a shadow child is hidden
    // behind its host.
    var root = this.getRootNode();
    var top = (root.elementFromPoint ? root : document).elementFromPoint(cx, cy);
    if (!top) return 'unknown';
    var tag = top.tagName.toLowerCase();
    if (top.id) tag += '#' + top.id;
    else if (top.className && typeof top.className === 'string') {
        var cls = top.className.trim().split(/\s+/).slice(0, 2).join('.');
        if (cls) tag += '.' + cls;
    }
    return tag;
}
