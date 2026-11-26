function() {
    var el = this;
    if (el.tagName === 'LABEL' && el.control) el = el.control;
    if (el.tagName === 'INPUT' && (el.type === 'checkbox' || el.type === 'radio'))
        return (el.type === 'radio' ? 'radio:' : 'checkbox:') + (el.checked ? 'true' : 'false');
    var role = el.getAttribute('role');
    if (role === 'checkbox' || role === 'switch' || role === 'menuitemcheckbox')
        return 'checkbox:' + (el.getAttribute('aria-checked') === 'true' ? 'true' : 'false');
    if (role === 'radio' || role === 'menuitemradio')
        return 'radio:' + (el.getAttribute('aria-checked') === 'true' ? 'true' : 'false');
    return null;
}
