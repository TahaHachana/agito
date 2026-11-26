function() {
    if (!this.isConnected) return false;
    function parentOf(node) {
        if (node.assignedSlot) return node.assignedSlot;
        if (node.parentElement) return node.parentElement;
        var root = node.parentNode;
        return root && root.host ? root.host : null;
    }
    function within(lo, hi, clipLo, clipHi) {
        if (hi - lo <= clipHi - clipLo) return lo >= clipLo && hi <= clipHi;
        var mid = (lo + hi) / 2;
        return mid >= clipLo && mid <= clipHi;
    }
    function shown(box, clip) {
        return within(box.left, box.right, clip.left, clip.right) &&
               within(box.top, box.bottom, clip.top, clip.bottom);
    }
    function inView(el) {
        var r = el.getBoundingClientRect();
        var box = { left: r.left, top: r.top, right: r.right, bottom: r.bottom };
        var node = el;
        var win = el.ownerDocument.defaultView;
        while (win) {
            var doc = win.document;
            var root = doc.documentElement;
            var rootStyle = win.getComputedStyle(root);
            var bodyIsViewport = rootStyle.overflowX === 'visible' && rootStyle.overflowY === 'visible';
            for (var p = parentOf(node); p; p = parentOf(p)) {
                if (p === root || (p === doc.body && bodyIsViewport)) continue;
                var s = win.getComputedStyle(p);
                var clipX = s.overflowX !== 'visible';
                var clipY = s.overflowY !== 'visible';
                if (!clipX && !clipY) continue;
                var pr = p.getBoundingClientRect();
                var left = pr.left + p.clientLeft;
                var top = pr.top + p.clientTop;
                var clip = {
                    left: clipX ? left : -Infinity, right: clipX ? left + p.clientWidth : Infinity,
                    top: clipY ? top : -Infinity, bottom: clipY ? top + p.clientHeight : Infinity
                };
                if (!shown(box, clip)) return false;
            }
            var vw = win.innerWidth, vh = win.innerHeight;
            if (doc.scrollingElement === root && root.clientWidth > 0 && root.clientHeight > 0) {
                vw = Math.min(vw, root.clientWidth);
                vh = Math.min(vh, root.clientHeight);
            }
            if (!shown(box, { left: 0, top: 0, right: vw, bottom: vh })) return false;
            var owner = null;
            try { owner = win.frameElement; } catch (e) { owner = null; }
            if (!owner) return true;
            var or = owner.getBoundingClientRect();
            var os = owner.ownerDocument.defaultView.getComputedStyle(owner);
            var dx = or.left + owner.clientLeft + (parseFloat(os.paddingLeft) || 0);
            var dy = or.top + owner.clientTop + (parseFloat(os.paddingTop) || 0);
            box = { left: box.left + dx, top: box.top + dy, right: box.right + dx, bottom: box.bottom + dy };
            node = owner;
            win = owner.ownerDocument.defaultView;
        }
        return true;
    }
    if (inView(this)) return false;
    this.scrollIntoView({ block: 'center', inline: 'center', behavior: 'instant' });
    return true;
}
