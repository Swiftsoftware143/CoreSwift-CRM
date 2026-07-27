# Private Email — In-App Walkthrough

This document contains the in-app wizard text and contextual help messages that appear as the user navigates through the Private Email setup in the CoreSwift UI. Each section maps to a specific screen or interaction.

---

## Domain Setup Screen

### Welcome (Empty State)

> Connect your email domain to send and receive emails from CoreSwift CRM. You'll be able to create professional addresses like `you@yourcompany.com` — and every email is automatically tracked against your contacts and deals.

### Step 1: Choose Your Provider

> Select how you want to send email. Not sure? **Mailgun** is the easiest — they handle everything for you. **SMTP** works with any email provider you already use.

**[Provider selection card appears after clicking "Add Domain"]**

#### If "Mailgun" is selected:

> **Mailgun manages email delivery for you.** Enter your Mailgun API key (find it in Mailgun Dashboard → Settings → API Keys). We'll verify your domain automatically — no DNS configuration needed on your end.
>
> New to Mailgun? [Sign up for a free account →](https://www.mailgun.com)
>
> **Already saved a key?** Select it from your saved API keys instead of entering it again.

#### If "SMTP" is selected:

> **Connect your existing email server.** You'll need these details from your email provider or IT administrator:
> - **SMTP Host** — e.g., `smtp.gmail.com` for Gmail, or your company's mail server
> - **Port** — typically 587 (recommended) or 465
> - **Username** — your email account login
> - **Password** — your email password or app-specific password
>
> **Important:** For SMTP, you manage the mailbox on your end. CoreSwift handles sending only. Make sure your DNS records (MX, SPF, DKIM) are set up with your provider.

#### If "SES" is selected:

> **Use Amazon SES for high-volume sending.** You'll need:
> - **AWS Access Key** — from IAM → Users → Security Credentials
> - **AWS Secret Key** — generated when you create the access key
> - Your domain must be verified in the SES console before you start.
>
> Best for businesses sending hundreds or thousands of emails per day.

#### If "Postmark" is selected:

> **Postmark specializes in fast transactional email.** Enter your Postmark Server API token (find it in your Postmark dashboard under Servers → API Tokens).
>
> Postmark is ideal for time-sensitive emails like password resets, order confirmations, and notifications.

### Step 2: Add Domain — Confirmation

#### Mailgun:

> ✓ **Connected!** We've verified your domain `{domain}` with Mailgun. Your domain is live and ready for mailboxes.

#### SMTP:

> ✓ **Domain saved.** Your SMTP configuration for `{domain}` has been stored. CoreSwift will use this to send emails. Remember to configure your mailboxes on your email server.

#### SES:

> ✓ **Domain saved.** Your AWS credentials for `{domain}` are stored. CoreSwift will send via Amazon SES. Verify your domain in the AWS SES console if you haven't already.

#### Postmark:

> ✓ **Domain saved.** Your Postmark token for `{domain}` is stored. CoreSwift will route outgoing emails through Postmark.

### Error: Invalid API Key (Mailgun)

> ✗ **Couldn't verify your domain.** The API key you entered didn't validate against Mailgun. Double-check:
> - Did you copy the full key from Mailgun Dashboard → Settings → API Keys?
> - Is your domain added in Mailgun's dashboard?
> - Are you using the correct region (US vs EU)?

### Error: Domain Limit Reached

> **Limit reached.** You've used {used} of {total} domains on your plan. Upgrade your plan to add more, or remove an existing domain first.

---

## Domain List Screen

### Empty State

> No domains connected yet. Add your first domain to start sending email from your own address.

### With Domains

> **{count} domain(s)** connected. Click a domain to view its mailboxes.

---

## Mailbox Creation Screen

### Before Creating (Empty State or Header)

> Create email addresses for your team and departments. Your plan allows up to **{limit}** mailboxes — you have **{used}** so far.

### Creating (Form)

> Enter the part before the @ symbol. For `john@yourcompany.com`, enter **john**. You can assign this mailbox to a team member so they can send and reply from this address.

#### After Creating — Mailgun Domain:

> ✓ **`{email}` is live.** Mailgun is routing emails to CoreSwift automatically. Incoming emails to this address will appear on your contacts' timelines.

#### After Creating — SMTP/SES/Postmark Domain:

> ✓ **`{email}` has been registered.** CoreSwift will use this address as the sender. Make sure this address exists on your email provider's side — CoreSwift handles sending only for non-Mailgun providers.

### Error: Mailbox Limit Reached

> **Mailbox limit reached.** You've used **{used}** of **{limit}** mailboxes. Upgrade your plan to add more, or remove an existing mailbox.

### Error: Address Already Exists

> **`{email}` already exists.** This email address is already in use. Choose a different local part.

---

## Mailbox List Screen

### Empty State

> No mailboxes yet. Create your first email address on your domain.

### With Mailboxes

> **{count} active mailbox(es).** Each mailbox can send email and track conversations in CoreSwift.

---

## Send Email Screen

### Composing

> Select which email address to send from. Replies to this email will automatically come back to CoreSwift and appear on the contact's timeline.

### After Sending

> ✓ **Queued for delivery.** Your email has been sent via **{provider}** and logged on the contact's timeline.
>
> `From: {from} → To: {to}`
> `Subject: {subject}`

### Error: Mailbox Not Active

> ✗ **Sending mailbox not found.** The selected email address is deactivated or doesn't exist. Choose another address or reactivate the mailbox.

### Error: Provider Send Failed

> ✗ **Delivery failed.** `{provider}` reported an error. Check your provider credentials and try again. If the problem persists, verify your provider account is in good standing.

---

## Auto-Reply Screen

### Creating an Auto-Reply

> Auto-replies send automatically when something happens in your CRM. Choose a trigger to get started.

### Trigger Selection

> **What should trigger this email?**
>
> - **Tag Added** — Fire when you tag a contact (e.g., tag a lead as `hot` → auto-send a pricing sheet)
> - **List Joined** — Fire when a contact subscribes to an email list
> - **Pipeline Stage** — Fire when a deal moves to a specific stage (e.g., "Proposal Sent" → follow-up email)
> - **Contact Created** — Fire immediately when a new contact is added to the CRM
> - **Always** — Fire for every incoming email to the selected mailbox (useful for out-of-office replies)

### Trigger Value (Refinement)

**[After selecting Tag Added / List Joined / Pipeline Stage]**

> Enter the specific trigger value — the exact tag name, list name, or pipeline stage. Leave blank to fire on any value.

### Delay Setting

> **When should this send?**
>
> - **0 minutes** = send immediately when the trigger fires
> - **Add a delay** to space out your communication — e.g., 60 minutes for a follow-up an hour after the trigger
>
> Delayed emails are queued and sent automatically. Use delays to build drip sequences.

### Body Editor

> Write your email content. HTML formatting is supported — use bold, links, and formatting to create professional-looking messages.
>
> The mailbox signature will be appended automatically — no need to add sign-off details here.

### After Creating

> ✓ **Auto-reply created.** It's **{active/inactive}** and will fire when `{trigger_type}` matches. Toggle it on/off from the list at any time.

---

## Auto-Reply List Screen

### Empty State

> No auto-replies yet. Create your first one to automate follow-ups, welcome emails, and engagement sequences.

### With Auto-Replies

> **{count} auto-replies.** Active ones fire automatically on matching CRM events. Delayed auto-replies are queued and sent at the scheduled time.

---

## Mailbox Settings Screen

### Signature

> This signature is appended to every email sent from `{email_address}`. Use it for your name, title, company, and contact details. HTML formatting is supported.
>
> **Example:**
> ```
> John Smith
> Customer Success Manager
> Acme Corp | acme.com
> 📞 (555) 123-4567
> ```

### Forwarding

> **When enabled**, all emails sent to `{email_address}` are also forwarded to the assigned team member's personal email. This ensures important messages are seen even outside of CoreSwift.

### Delete Mailbox

> **Deleting soft-deactivates the mailbox.** It won't send or receive new emails, but your email history remains on contact timelines. You can recreate the address later if needed.

---

## API Keys Screen

### Before Adding

> Save your provider API keys once and reuse them across domains. Keys are encrypted in our database.

### Adding a Key

> **Label** — A name to identify this key (e.g., "Production Mailgun")
> **Provider** — Which service this key is for
> **API Key / Token** — Paste your key from the provider's dashboard

### With Keys

> **{count} saved key(s).** Select them when adding a domain to avoid re-entering credentials. Keys are stored encrypted.

---

## Catch-All Setting (Pro Plan+)

### Enabling Catch-All

> **Catch-all routing** captures emails sent to any address on your domain, even if no specific mailbox exists. Emails to `anything@yourdomain.com` will be accepted and routed.
>
> ⚠️ **Pro plan or higher required.** Catch-all is not available on Starter plans.

---

## General Error Messages

### Feature Not Available

> **Private Email is not available on your plan.** Upgrade to a plan that includes Private Email to connect your domain and create mailboxes.

### Not Authorized

> You don't have permission to manage email settings. Contact your account administrator.

### Domain Not Found

> The domain you're looking for doesn't exist or was removed. Return to the domains list.

### Mailbox Not Found

> This mailbox doesn't exist or has been deactivated. Return to the mailboxes list.
