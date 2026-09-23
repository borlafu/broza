// Tabs and copy buttons. Both are progressive: with no JavaScript every
// install panel is visible and every command is still selectable text.

(function () {
  'use strict';

  var list = document.getElementById('install-tabs');
  if (!list) return;

  var tabs = Array.prototype.slice.call(list.querySelectorAll('[role="tab"]'));
  var panels = tabs.map(function (tab) {
    return document.getElementById(tab.getAttribute('aria-controls'));
  });

  function select(index, focus) {
    tabs.forEach(function (tab, i) {
      tab.setAttribute('aria-selected', String(i === index));
      tab.tabIndex = i === index ? 0 : -1;
      panels[i].hidden = i !== index;
    });
    if (focus) tabs[index].focus();
  }

  tabs.forEach(function (tab, i) {
    tab.addEventListener('click', function () { select(i, false); });
    tab.addEventListener('keydown', function (event) {
      var step = event.key === 'ArrowRight' ? 1 : event.key === 'ArrowLeft' ? -1 : 0;
      if (!step) return;
      event.preventDefault();
      select((i + step + tabs.length) % tabs.length, true);
    });
  });

  list.hidden = false;
  select(0, false);
})();

(function () {
  'use strict';

  // execCommand is deprecated but it is the only path that works where the
  // async clipboard is blocked (a page served over plain http, a browser that
  // has not granted the permission).
  function legacyCopy(text) {
    var field = document.createElement('textarea');
    field.value = text;
    field.setAttribute('readonly', '');
    field.style.cssText = 'position:absolute;left:-9999px;top:0';
    document.body.appendChild(field);
    field.select();
    var copied = false;
    try { copied = document.execCommand('copy'); } catch (error) { copied = false; }
    document.body.removeChild(field);
    return copied;
  }

  function attach(button, read) {
    var label = button.firstChild;
    var original = label.textContent;
    var revert = null;

    function report(copied) {
      label.textContent = copied ? 'copied' : 'press ⌘C';
      if (revert) clearTimeout(revert);
      revert = setTimeout(function () { label.textContent = original; }, 1600);
    }

    button.addEventListener('click', function () {
      var text = read();
      if (!navigator.clipboard) { report(legacyCopy(text)); return; }
      navigator.clipboard.writeText(text).then(function () {
        report(true);
      }, function () {
        report(legacyCopy(text));
      });
    });
  }

  Array.prototype.forEach.call(document.querySelectorAll('[data-copy-text]'), function (button) {
    attach(button, function () { return button.dataset.copyText; });
  });

  Array.prototype.forEach.call(document.querySelectorAll('[data-copy]'), function (button) {
    var source = document.getElementById(button.dataset.copy);
    if (source) attach(button, function () { return source.textContent; });
  });
})();
