# SPDX-License-Identifier: MPL-2.0
#
# What a person types when uninstalling Koru on Windows, sent to the same
# feedback endpoint the app's Send feedback form posts to (ADR-275,
# apps/koru/src/feedback/submit.rs). Run by the uninstaller
# (koru-installer.nsi), which packs this file; it does not use Koru's own
# executables, so feedback still goes when Koru itself is what is broken.
#
# The uninstaller hands the text over in the environment, never on the
# command line, so nothing typed needs quoting:
#   KORU_FEEDBACK_MESSAGE  what was typed (at least three characters)
#   KORU_FEEDBACK_CONTACT  an email to reply to, or empty
#   KORU_FEEDBACK_PRODUCT  "Koru Nightly <version>", said in the message
#   KORU_FEEDBACK_URL      optional, as for the app: another endpoint
#
# The form is the app's: `kind`, `message`, optional `contact`, and the empty
# `website` honeypot the route treats a filled one of as a bot. There is no
# uninstall kind on the route, so it is `general` and the message says it.
# A failure exits non-zero and the uninstall goes on regardless.

$ErrorActionPreference = 'Stop'

$text = "$env:KORU_FEEDBACK_MESSAGE".Trim()
if ($text.Length -lt 3) { exit 0 }
$contact = "$env:KORU_FEEDBACK_CONTACT".Trim()
$message = "Uninstalled $env:KORU_FEEDBACK_PRODUCT.`r`n`r`n$text"
# The route refuses the whole report over a contact that is not an email;
# keeping what was typed in the message keeps the report.
if ($contact -and $contact -notmatch '^[^@\s]+@[^@\s]+\.[^@\s]+$') {
    $message += "`r`n`r`nContact, as typed: $contact"
    $contact = ''
}

$url = "$env:KORU_FEEDBACK_URL".Trim()
if (-not $url) { $url = 'https://www.koru.do/api/feedback' }
# The apex answers 308, which a multipart POST cannot follow (submit.rs).
$url = $url -replace '://koru\.do/', '://www.koru.do/'

Add-Type -AssemblyName System.Net.Http
[Net.ServicePointManager]::SecurityProtocol = [Net.ServicePointManager]::SecurityProtocol -bor [Net.SecurityProtocolType]::Tls12
# Names in quotes: .NET writes them bare otherwise (`name=kind`), which
# RFC 7578 does not allow and a strict parser refuses.
$form = New-Object System.Net.Http.MultipartFormDataContent
$form.Add((New-Object System.Net.Http.StringContent 'general'), '"kind"')
$form.Add((New-Object System.Net.Http.StringContent $message), '"message"')
if ($contact) {
    $form.Add((New-Object System.Net.Http.StringContent $contact), '"contact"')
}
$form.Add((New-Object System.Net.Http.StringContent ''), '"website"')
$client = New-Object System.Net.Http.HttpClient
$client.Timeout = [TimeSpan]::FromSeconds(15)
$response = $client.PostAsync($url, $form).GetAwaiter().GetResult()
if (-not $response.IsSuccessStatusCode) {
    Write-Error "Feedback was not accepted: $([int]$response.StatusCode)"
    exit 1
}
