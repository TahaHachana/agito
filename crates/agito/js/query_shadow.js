function queryShadow(root, sel) {
    var results = Array.from(root.querySelectorAll(sel));
    var all = root.querySelectorAll('*');
    for (var i = 0; i < all.length; i++) {
        var sr = all[i].shadowRoot;
        if (sr) results = results.concat(queryShadow(sr, sel));
    }
    return results;
}
