// CRMSwift Plan Configuration
// Define features available on each plan tier

export interface PlanFeature {
  id: string;
  name: string;
  description: string;
  defaultValue: boolean | number;
}

export interface PlanConfig {
  id: string;
  name: string;
  price: number;
  billing: 'monthly' | 'yearly';
  features: Record<string, boolean | number>;
}

// All available features in the system
export const AVAILABLE_FEATURES: PlanFeature[] = [
  { id: 'pipelines', name: 'Pipelines', description: 'Number of sales pipelines', defaultValue: 1 },
  { id: 'users', name: 'Team Members', description: 'Number of users', defaultValue: 1 },
  { id: 'contacts', name: 'Contacts', description: 'Max contacts stored', defaultValue: 100 },
  { id: 'deals', name: 'Active Deals', description: 'Max active deals', defaultValue: 50 },
  { id: 'automations', name: 'Automations', description: 'n8n workflows enabled', defaultValue: false },
  { id: 'api_access', name: 'API Access', description: 'REST API & webhooks', defaultValue: false },
  { id: 'custom_fields', name: 'Custom Fields', description: 'Custom deal/contact fields', defaultValue: false },
  { id: 'reports', name: 'Advanced Reports', description: 'Detailed analytics', defaultValue: false },
  { id: 'integrations', name: 'Integrations', description: 'ADASwift, MissedCall, WorkflowSwift', defaultValue: false },
  { id: 'affiliate_tracking', name: 'Affiliate Tracking', description: 'Track affiliate referrals', defaultValue: false },
  { id: 'white_label', name: 'White Label', description: 'Remove CRMSwift branding', defaultValue: false },
  { id: 'priority_support', name: 'Priority Support', description: 'Fast support response', defaultValue: false },
];

// Default plan configurations
export const DEFAULT_PLANS: PlanConfig[] = [
  {
    id: 'starter',
    name: 'Starter',
    price: 29,
    billing: 'monthly',
    features: {
      pipelines: 1,
      users: 1,
      contacts: 500,
      deals: 100,
      automations: false,
      api_access: false,
      custom_fields: false,
      reports: false,
      integrations: false,
      affiliate_tracking: false,
      white_label: false,
      priority_support: false,
    },
  },
  {
    id: 'pro',
    name: 'Professional',
    price: 79,
    billing: 'monthly',
    features: {
      pipelines: 3,
      users: 5,
      contacts: 5000,
      deals: 1000,
      automations: true,
      api_access: true,
      custom_fields: true,
      reports: true,
      integrations: true,
      affiliate_tracking: false,
      white_label: false,
      priority_support: false,
    },
  },
  {
    id: 'agency',
    name: 'Agency',
    price: 199,
    billing: 'monthly',
    features: {
      pipelines: 10,
      users: 25,
      contacts: 50000,
      deals: 10000,
      automations: true,
      api_access: true,
      custom_fields: true,
      reports: true,
      integrations: true,
      affiliate_tracking: true,
      white_label: true,
      priority_support: true,
    },
  },
];

// Check if feature is enabled for current plan
export function hasFeature(
  planFeatures: Record<string, boolean | number>,
  featureId: string,
  value?: number
): boolean {
  const feature = planFeatures[featureId];
  
  if (typeof feature === 'boolean') {
    return feature;
  }
  
  if (typeof feature === 'number' && value !== undefined) {
    return feature >= value;
  }
  
  return false;
}

// Get feature limit (for numeric features like contacts, deals)
export function getFeatureLimit(
  planFeatures: Record<string, boolean | number>,
  featureId: string
): number {
  const feature = planFeatures[featureId];
  return typeof feature === 'number' ? feature : 0;
}
