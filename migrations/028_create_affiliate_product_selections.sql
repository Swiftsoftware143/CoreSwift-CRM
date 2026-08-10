-- 028_create_affiliate_product_selections.sql
-- Affiliates self-select which products to promote from their FunnelSwift back-end
-- Tracks which affiliate is promoting which product

CREATE TABLE IF NOT EXISTS affiliate_product_selections (
    id UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
    affiliate_id UUID NOT NULL REFERENCES affiliates(id) ON DELETE CASCADE,
    product_id UUID NOT NULL REFERENCES affiliate_products(id) ON DELETE CASCADE,
    is_active BOOLEAN DEFAULT true,
    promo_link TEXT,
    custom_commission_rate DECIMAL(5,2),
    selected_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    UNIQUE(affiliate_id, product_id)
);

CREATE INDEX IF NOT EXISTS idx_aps_affiliate ON affiliate_product_selections(affiliate_id);
CREATE INDEX IF NOT EXISTS idx_aps_product ON affiliate_product_selections(product_id);
CREATE INDEX IF NOT EXISTS idx_aps_active ON affiliate_product_selections(affiliate_id, is_active);
