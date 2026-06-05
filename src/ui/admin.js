document.addEventListener('click', function (e) {
  var btn = e.target.closest('.kv-delete-btn');
  if (btn) {
    e.preventDefault();
    var partition = btn.dataset.partition;
    var key = btn.dataset.key;
    if (!confirm('Delete "' + key + '" from partition "' + partition + '"?')) return;
    var partitionPath = partition.split('/').map(encodeURIComponent).join('/');
    btn.disabled = true;
    fetch('/admin/db/' + partitionPath + '?key=' + encodeURIComponent(key), { method: 'DELETE' })
      .then(function (r) {
        if (r.ok) {
          var entry = btn.closest('.kv-entry');
          if (entry) entry.remove();
        } else {
          btn.disabled = false;
          r.text().then(function (t) { alert('Delete failed: ' + t); });
        }
      })
      .catch(function (err) { btn.disabled = false; alert('Error: ' + err); });
    return;
  }

  btn = e.target.closest('.queue-trigger-btn');
  if (btn) {
    e.preventDefault();
    var partition = btn.dataset.partition;
    var key = btn.dataset.key;
    var payloadStr = btn.dataset.payload;
    var payload;
    try { payload = JSON.parse(payloadStr); } catch (_) { payload = payloadStr; }
    var partitionPath = partition.split('/').map(encodeURIComponent).join('/');
    btn.disabled = true;
    fetch('/admin/queue/' + partitionPath, {
      method: 'PUT',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ key: key, payload: payload })
    })
      .then(function (r) {
        if (r.ok) {
          var entry = btn.closest('.queue-entry');
          if (entry) {
            var st = entry.querySelector('.queue-status');
            if (st) { st.className = 'queue-status status-pending'; st.textContent = 'Pending'; }
          }
        } else {
          btn.disabled = false;
          r.text().then(function (t) { alert('Trigger failed: ' + t); });
        }
      })
      .catch(function (err) { btn.disabled = false; alert('Error: ' + err); });
  }
});

