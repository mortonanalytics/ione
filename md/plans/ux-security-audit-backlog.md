# UX + Security Audit Backlog

Status: tracked follow-up after the P0-P2 remediation pass.

## P3 Security

- Normalize the `Secure` cookie flag across session issuance paths so all production cookies share one policy.
- Resolve the RLS false-security signal by either wiring request-scoped `SET LOCAL app.current_org_id` for protected tables or disabling RLS where app-layer predicates remain the real isolation boundary.
- Add rate limiting to the MFA challenge endpoint.
- Review `whoami://` disclosure and either document the current email/role exposure or scope-gate it.

## P3 UX

- Move hardcoded status badge colors into reusable CSS tokens.
- Finish auth-surface format hints and ARIA polish.
