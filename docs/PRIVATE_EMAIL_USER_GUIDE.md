# Private Email — User Guide

## Overview

Private Email lets you send and receive email from professional addresses on your own domain — like `support@yourcompany.com` or `sales@yourcompany.com` — directly inside CoreSwift CRM. No need to switch between your email app and your CRM. Every email is tracked against your contacts and deals automatically.

**What you get:**
- Send and receive email from addresses on your domain
- Auto-reply sequences triggered by CRM events
- Email tracking against contacts and deals
- Multiple provider options to fit your setup

---

## Step 1: Connect Your Domain

Before creating email addresses, you need to connect your domain to CoreSwift.

### Prerequisites

- You own a domain name (e.g., `yourcompany.com`)
- You have access to your domain's DNS settings (usually through your domain registrar or hosting provider)
- You have an account with one of our supported email providers

### Supported Providers

CoreSwift works with four email providers. Choose the one that fits your situation:

| Provider | Best For | What You Need |
|----------|----------|---------------|
| **Mailgun** | Easiest setup — Mailgun handles everything | A Mailgun API key (free tier available) |
| **SMTP** | Using your existing email server or provider | Host, port, username, and password |
| **SES (Amazon)** | High-volume sending | AWS access key and secret |
| **Postmark** | Transactional email | Postmark server API token |

#### Option A: Mailgun (Recommended for Easy Setup)

Mailgun is the simplest option. Mailgun manages email delivery, routing, and domain verification for you.

1. **Sign up** for a Mailgun account at [mailgun.com](https://www.mailgun.com) if you don't have one
2. **Add your domain** in Mailgun's dashboard (Mailgun will give you DNS records to configure)
3. **Get your API key** from Mailgun Dashboard → Settings → API Keys
4. In CoreSwift, go to **Email → Domains → Add Domain**
5. Select **Mailgun** as the provider
6. Enter your domain name and API key
7. Click **Add Domain**

CoreSwift will verify your domain with Mailgun automatically. Once verified, your domain is ready to use.

> **Tip:** You can save your API key under **Email → API Keys** first, then select it from a dropdown when adding a domain. This way you only enter it once.

#### Option B: SMTP (Your Own Email Server)

Use this if you already have an email server or provider (Gmail, Outlook, custom SMTP server).

1. In CoreSwift, go to **Email → Domains → Add Domain**
2. Select **SMTP** as the provider
3. Enter your domain name
4. Fill in your SMTP connection details:
   - **SMTP Host** — e.g., `smtp.gmail.com` for Gmail
   - **Port** — typically 587 (TLS) or 465 (SSL)
   - **Username** — your email account username
   - **Password** — your email account password or app-specific password
5. Click **Add Domain**

> **Important:** For SMTP domains, CoreSwift handles sending only. You must manage your DNS records (MX, SPF, DKIM) on your provider's side. Mailboxes on your email server must be configured there separately.

#### Option C: SES (Amazon Simple Email Service)

Use this for high-volume sending through Amazon's infrastructure.

1. Create an SES account and verify your domain in the AWS console
2. Generate an IAM user with SES send permissions
3. In CoreSwift, go to **Email → Domains → Add Domain**
4. Select **SES** as the provider
5. Enter your domain name and AWS credentials
6. Click **Add Domain**

#### Option D: Postmark

Postmark specializes in fast, reliable transactional email delivery.

1. Get your Postmark Server API token from your Postmark dashboard
2. In CoreSwift, go to **Email → Domains → Add Domain**
3. Select **Postmark** as the provider
4. Enter your domain name and API token
5. Click **Add Domain**

### Domain Verification

- **Mailgun domains** are verified automatically when you add them — CoreSwift checks your API key against Mailgun.
- **SMTP/SES/Postmark domains** are saved immediately. You are responsible for configuring DNS records (MX, SPF, DKIM) on your provider's side. CoreSwift uses your domain for sending outgoing email regardless.

### Plan Limits

Your plan determines how many domains you can connect. You'll see your current usage displayed — e.g., "1 of 3 domains used." If you reach your limit, you'll need to upgrade your plan or contact your account administrator.

---

## Step 2: Create Email Addresses (Mailboxes)

After connecting a domain, you can create individual email addresses — called **mailboxes** — for your team.

### What Is a Mailbox?

A mailbox is an email address on your domain, like:
- `support@yourcompany.com`
- `sales@yourcompany.com`
- `john@yourcompany.com`

### Creating a Mailbox

1. Go to **Email → Mailboxes → New Mailbox**
2. Select the domain from the dropdown
3. Enter the **local part** — the part before the `@` symbol (e.g., enter `support` for `support@yourcompany.com`)
4. Optionally, assign it to a team member so they can send and reply from this address
5. Click **Create Mailbox**

### What Happens Behind the Scenes

- **Mailgun domains:** CoreSwift creates the mailbox and configures a route with Mailgun so incoming emails are forwarded to CoreSwift automatically. The address is live immediately.
- **SMTP/SES/Postmark domains:** CoreSwift registers the address. Make sure the email address is configured on your email provider's side as well. Emails sent from CoreSwift will use this address as the sender.

### Mailbox Limits

Your plan sets how many mailboxes you can create. You'll see a counter like "3 of 10 mailboxes used." When you hit the limit, creating new mailboxes will be blocked until you upgrade or remove an existing one.

---

## Step 3: Send Emails

You can send emails directly from the Contacts page or the Deal view.

### Composing an Email

1. Open a contact or deal
2. Click **Send Email**
3. Choose the **From** address — any mailbox assigned to you or available on your domain
4. Enter the recipient (pre-filled if from a contact page)
5. Write your subject and message
6. Click **Send**

### What You Can Do

- **Rich text formatting** — bold, italic, lists, and links in your email body
- **Reply tracking** — replies to your email automatically thread back into the contact's timeline
- **Signature** — your mailbox signature is automatically appended to every email
- **Contact events** — each sent email is logged on the contact's timeline so you can see your full communication history

### After Sending

Your email is queued and sent through your domain's configured provider. Check the contact's timeline for delivery status. If the recipient replies, the inbound email flows back into CoreSwift and appears on the contact's timeline automatically.

---

## Step 4: Auto-Replies

Auto-replies let you send emails automatically when something happens in your CRM. Think of them as automated follow-ups, welcome messages, or engagement sequences.

### What Triggers an Auto-Reply?

When you create an auto-reply, you choose a trigger — the event that fires the email:

| Trigger | Fires When… | Use Case Example |
|---------|-------------|-----------------|
| **Tag Added** | A specific tag is added to a contact | "Thanks for registering!" when `new-lead` tag is applied |
| **List Joined** | A contact joins an email list | "Welcome to our newsletter" |
| **Pipeline Stage** | A deal moves to a specific stage | "Your proposal is ready" when deal hits "Proposal Sent" stage |
| **Contact Created** | A new contact is added | "Thanks for getting in touch" |
| **Always** | Every inbound email to the mailbox | Out-of-office or acknowledgment replies |

### Creating an Auto-Reply

1. Go to **Email → Auto-Replies → New Auto-Reply**
2. Give it a **name** (e.g., "Welcome New Lead")
3. Select which **domain** and optionally which **mailbox** it applies to
4. Choose the **trigger type** (Tag Added, Pipeline Stage, etc.)
5. Set the **trigger value** — the specific tag name, list name, or stage name (leave blank for "any")
6. Write your **subject** and **body** (HTML supported)
7. Set the **delay** (see below)
8. Click **Create**

### Setting a Delay

- **0 minutes** = send immediately when the trigger fires
- **Add a delay** to create a drip sequence — e.g., 60 minutes = the email goes out one hour after the trigger fires
- Delayed emails are queued and automatically sent at the scheduled time

### Managing Auto-Replies

From the Auto-Replies list, you can:
- **Toggle on/off** — disable an auto-reply without deleting it
- **Edit** — change the subject, body, or delay
- **Delete** — remove the auto-reply permanently

---

## Step 5: Managing Mailboxes

### Edit Signature

Your mailbox signature is appended to every email sent from that address.

1. Go to **Email → Mailboxes**
2. Click on a mailbox to open its settings
3. Edit the **Signature** field
4. Click **Save**

Use this for your name, title, company info, and any standard closing text. HTML formatting is supported.

### Toggle Forwarding

When forwarding is enabled, all emails sent to this address are also forwarded to the assigned team member's personal email.

1. Go to the mailbox settings
2. Toggle **Forwarding** on or off
3. Click **Save**

### Delete a Mailbox

Deleting a mailbox soft-deactivates it — the address still exists in your records but is marked as inactive and won't send or receive.

1. Go to **Email → Mailboxes**
2. Click the delete icon on the mailbox
3. Confirm the deletion

> **Note:** Deleted mailboxes free up your plan's mailbox count so you can create new ones.

---

## Saving API Keys

To avoid entering your Mailgun API key every time you add a domain, you can save it once:

1. Go to **Email → API Keys → Add Key**
2. Enter a **label** (e.g., "Production Mailgun Key")
3. Select the **provider** (Mailgun, SES, Postmark)
4. Paste your API key or token
5. Click **Save**

Now when you add a domain, you can select your saved key from a dropdown instead of typing it again. Your API keys are encrypted in the database.

---

## FAQs

### Can I use my existing Gmail or Outlook account?

Yes. Choose the **SMTP** provider option and enter your Gmail/Outlook SMTP settings. For Gmail, you may need to generate an app-specific password from your Google Account security settings.

### Does Mailgun cost money?

Mailgun offers a free tier that includes basic sending and receiving. For higher volumes, paid plans are available. Check [mailgun.com/pricing](https://www.mailgun.com/pricing) for current details.

### Can I have multiple domains?

Yes, if your plan supports it. Each plan has a maximum number of domains. You'll see how many you've used and how many are available on the Domains page.

### What happens to emails when I delete a mailbox?

The mailbox is soft-deactivated. No new emails will be sent or received from that address, but your history and email records are preserved in the contact timelines.

### How do I set up DNS records for SMTP/SES/Postmark?

CoreSwift doesn't manage DNS for non-Mailgun providers. You'll need to configure MX, SPF, and DKIM records through your domain registrar or DNS provider. Your email provider's documentation should have the exact values.

### Can I receive emails into CoreSwift?

Yes, for Mailgun domains, inbound emails are automatically routed to CoreSwift and appear on the matching contact's timeline. If no matching contact exists, one is automatically created. For SMTP/SES/Postmark domains, inbound email handling depends on your provider's configuration.

### Who can send from a mailbox?

Any team member assigned to the mailbox can send from it. Unassigned mailboxes are available to all team members in your organization.

### How do I upgrade my limits?

Contact your account administrator or visit the billing section of your account to upgrade your plan. Agency admins can also set custom limits for any tenant.
