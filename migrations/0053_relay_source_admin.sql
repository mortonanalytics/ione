UPDATE roles
SET permissions = permissions || '["data:sources:write"]'::jsonb
WHERE coc_level >= 80
  AND NOT (permissions @> '["data:sources:write"]'::jsonb);
