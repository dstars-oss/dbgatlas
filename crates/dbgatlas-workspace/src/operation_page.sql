WITH overrides AS MATERIALIZED (
    SELECT key AS id, value AS patch FROM json_each(?2)
),
candidates AS (
    SELECT e.id, e.created,
        CASE WHEN o.id IS NOT NULL THEN json_patch(e.record,o.patch)
             WHEN e.status IN ('pending','running') THEN json_set(e.record,'$.status','unknown')
             ELSE e.record END AS value
    FROM executions e LEFT JOIN overrides o ON o.id=e.id
    WHERE e.workspace_id=?1 /*scope*/
),
filtered AS (
    SELECT id,created,value FROM candidates
    WHERE (?4='' OR json_extract(value,'$.status')=?4)
      AND (?5='' OR instr(lower(id || ' ' || json_extract(value,'$.capability') || ' ' ||
          json_extract(value,'$.summary') || ' ' || coalesce(json_extract(value,'$.input'),'')),lower(?5))>0)
)
SELECT (SELECT count(*) FROM filtered), coalesce((
    SELECT json_group_array(json(value)) FROM (
        SELECT value FROM filtered ORDER BY created DESC,id DESC LIMIT ?8 OFFSET ?9
    )
),'[]')
