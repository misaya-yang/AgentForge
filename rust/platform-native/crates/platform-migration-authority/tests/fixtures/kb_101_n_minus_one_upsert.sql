
INSERT INTO segments (
    segment_id, dataset_id, document_id, position, text, content_type
) VALUES ($1, 'dataset-a', 'document-a', 0, $2, 'text')
ON CONFLICT (document_id, position)
DO UPDATE SET text = EXCLUDED.text
