function() {
    var style = window.getComputedStyle(this);
    if (style.display === 'none') return false;
    if (style.visibility === 'hidden') return false;
    if (parseFloat(style.opacity) === 0) return false;
    var r = this.getBoundingClientRect();
    return r.width > 0 && r.height > 0;
}
