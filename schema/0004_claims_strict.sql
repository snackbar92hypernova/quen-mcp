-- quen ledger, migration 0004: a stricter claims_status. crates/quen-core/src/ledger.rs runs it
-- in one transaction while PRAGMA user_version is 3. Conventions as in 0001.
--
-- 0001 let one family's newer support clear every older contradiction, so a claim could stay proven on a second
-- family whose only support predated the contradiction. Now a contradiction (supports = 0, any tag) is outranked only
-- when at least two distinct method families have strong support (tag M, O or static_re) from boots that started
-- after its boot. A contradiction from no boot, or one that fewer than two families support after (the same boot
-- isn't after), stands. A claim is proven when it is open, has strong support from at least two families, and no
-- contradiction stands: after the newest contradiction, two independent methods must agree again.

DROP VIEW claims_status;

CREATE VIEW claims_status AS
WITH evidence AS (
  SELECT s.claim_id, s.supports, m.method_family, m.tag IN ('M', 'O', 'static_re') AS strong, b.t_start
  FROM claim_support s
  JOIN measurements m ON m.id = s.measurement_id
  LEFT JOIN boots b ON b.id = m.boot_id
),
support AS (
  SELECT claim_id, count(DISTINCT method_family) AS families
  FROM evidence
  WHERE supports = 1 AND strong
  GROUP BY claim_id
),
standing AS (
  SELECT e.claim_id, count(*) AS contradictions
  FROM evidence e
  WHERE e.supports = 0
    AND (e.t_start IS NULL
         OR (SELECT count(DISTINCT n.method_family) FROM evidence n
             WHERE n.claim_id = e.claim_id AND n.supports = 1 AND n.strong AND n.t_start > e.t_start) < 2)
  GROUP BY e.claim_id
)
SELECT
  c.id,
  c.text,
  c.scope,
  c.status,
  c.withdrawn_reason,
  coalesce(s.families, 0) AS families,
  coalesce(k.contradictions, 0) AS contradictions,
  c.status = 'open' AND coalesce(s.families, 0) >= 2 AND k.contradictions IS NULL AS proven
FROM claims c
LEFT JOIN support s ON s.claim_id = c.id
LEFT JOIN standing k ON k.claim_id = c.id;
