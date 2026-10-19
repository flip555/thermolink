# Alarms and recording

## Alarms

![Alarms tab](images/ui-alarms.png)

1. Open **Alarms**.
2. Choose **Above** or **Below**, enter the threshold in °C and a dwell time in
   seconds.
3. Give it a name (optional) and press **Add alarm**.
4. Add a Discord/Slack/Telegram/webhook destination under **Notifications** and
   press **Save**. Blank fields are left unchanged; stored values are never
   returned to the page.

**Recent events** and **Last event** show the rule, temperature and time. Alarm
state is persisted in the data directory.

## Snapshots

![Capture tab](images/ui-capture.png)

**Save snapshot** writes a JPEG, raw frame, JSON metadata and, for Qianli, the
visible still. Snapshots appear in the list with `jpg raw json vis` links.
**re** opens one in the main viewer (see [Using the view](Using-the-view)).

**Pre-record** keeps a rolling buffer; set the before/after seconds and press
**Save**. The ring is written when an alarm fires or a capture is requested.
`captures.zip` downloads the capture directory.

## References and ΔT

Under **References**, save the current frame with a name, press **●** to
activate it, then tick **Show ΔT image** in the Capture tab. The difference view
uses the configured span; it is a live comparison, not a snapshot.
