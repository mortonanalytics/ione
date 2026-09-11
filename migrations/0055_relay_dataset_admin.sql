UPDATE roles
SET permissions = permissions || '["data:datasets:write"]'::jsonb
WHERE coc_level >= 80
  AND NOT (permissions @> '["data:datasets:write"]'::jsonb);
