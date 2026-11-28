// A CSS selector for an element: its id when that is unique, otherwise a path of tag names,
// with :nth-of-type where siblings share one, up to the nearest ancestor with a unique id.
function cssPath(el) {
    var root = el.getRootNode();
    function unique(id) {
        return id && root.querySelectorAll('#' + CSS.escape(id)).length === 1;
    }
    var parts = [];
    for (var node = el; node && node.nodeType === 1; node = node.parentElement) {
        if (unique(node.id)) {
            parts.unshift('#' + CSS.escape(node.id));
            break;
        }
        var part = node.localName;
        var parent = node.parentElement;
        if (parent) {
            var same = Array.prototype.filter.call(parent.children, function (c) {
                return c.localName === node.localName;
            });
            if (same.length > 1) part += ':nth-of-type(' + (same.indexOf(node) + 1) + ')';
        }
        parts.unshift(part);
    }
    return parts.join(' > ');
}
