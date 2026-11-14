//! Pure pricing logic: no database, fully unit-tested.

use super::models::{Bundle, Quote};
use uuid::Uuid;

#[derive(Debug, Clone, Copy)]
pub struct LineItem {
    pub resource_id: Uuid,
    pub price_cents: i64,
}

/// Discount a single bundle gives on these items (0 if it does not apply).
pub fn bundle_discount(items: &[LineItem], b: &Bundle) -> i64 {
    if !b.active || b.min_items <= 0 {
        return 0;
    }
    if !b
        .required_resource_ids
        .iter()
        .all(|r| items.iter().any(|i| i.resource_id == *r))
    {
        return 0;
    }
    let mut eligible: Vec<i64> = items
        .iter()
        .filter(|i| {
            b.eligible_resource_ids.is_empty() || b.eligible_resource_ids.contains(&i.resource_id)
        })
        .map(|i| i.price_cents)
        .collect();
    let n = b.min_items as usize;
    if eligible.len() < n {
        return 0;
    }
    match b.kind.as_str() {
        "percent_off" => eligible.iter().sum::<i64>() * b.percent_off as i64 / 100,
        "fixed_price" => {
            // Group the most expensive items first so the customer gets the best deal.
            eligible.sort_unstable_by(|a, b| b.cmp(a));
            eligible
                .chunks_exact(n)
                .map(|group| (group.iter().sum::<i64>() - b.fixed_price_cents).max(0))
                .sum()
        }
        _ => 0,
    }
}

/// Price an order, applying the single bundle that saves the customer the most.
pub fn quote(items: &[LineItem], bundles: &[Bundle]) -> Quote {
    let subtotal: i64 = items.iter().map(|i| i.price_cents).sum();
    let best = bundles
        .iter()
        .map(|b| (bundle_discount(items, b), b))
        .filter(|(d, _)| *d > 0)
        .max_by_key(|(d, _)| *d);
    let (discount, bundle) = match best {
        Some((d, b)) => (d.min(subtotal), Some(b)),
        None => (0, None),
    };
    Quote {
        subtotal_cents: subtotal,
        discount_cents: discount,
        total_cents: subtotal - discount,
        bundle_id: bundle.map(|b| b.id),
        bundle_name: bundle.map(|b| b.name.clone()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bundle(kind: &str, min_items: i32, percent: i32, fixed: i64) -> Bundle {
        Bundle {
            id: Uuid::new_v4(),
            calendar_id: Uuid::new_v4(),
            name: format!("{kind}-{min_items}"),
            kind: kind.into(),
            min_items,
            percent_off: percent,
            fixed_price_cents: fixed,
            eligible_resource_ids: vec![],
            required_resource_ids: vec![],
            active: true,
        }
    }

    fn items(res: Uuid, prices: &[i64]) -> Vec<LineItem> {
        prices
            .iter()
            .map(|p| LineItem {
                resource_id: res,
                price_cents: *p,
            })
            .collect()
    }

    #[test]
    fn no_bundle_means_no_discount() {
        let q = quote(&items(Uuid::new_v4(), &[10_000, 5_000]), &[]);
        assert_eq!(
            (q.subtotal_cents, q.discount_cents, q.total_cents),
            (15_000, 0, 15_000)
        );
        assert!(q.bundle_id.is_none());
    }

    #[test]
    fn percent_off_needs_minimum_items() {
        let b = bundle("percent_off", 4, 20, 0);
        let r = Uuid::new_v4();
        assert_eq!(
            quote(&items(r, &[10_000; 3]), std::slice::from_ref(&b)).discount_cents,
            0
        );
        let q = quote(&items(r, &[10_000; 4]), &[b]);
        assert_eq!((q.discount_cents, q.total_cents), (8_000, 32_000));
    }

    #[test]
    fn fixed_price_groups_most_expensive_and_leaves_remainder_full_price() {
        // "Any 4 for $300": 5 items -> one group of the 4 priciest, 1 left at full price.
        let b = bundle("fixed_price", 4, 0, 30_000);
        let q = quote(
            &items(Uuid::new_v4(), &[15_000, 10_000, 10_000, 10_000, 5_000]),
            &[b],
        );
        assert_eq!(q.subtotal_cents, 50_000);
        assert_eq!(q.discount_cents, 15_000); // 45,000 group -> 30,000
        assert_eq!(q.total_cents, 35_000);
    }

    #[test]
    fn fixed_price_never_raises_the_price() {
        let b = bundle("fixed_price", 2, 0, 30_000);
        assert_eq!(
            quote(&items(Uuid::new_v4(), &[5_000, 5_000]), &[b]).discount_cents,
            0
        );
    }

    #[test]
    fn eligible_and_required_resources_are_respected() {
        let (sponsor, classified, directory) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
        let mut b = bundle("percent_off", 1, 50, 0);
        b.eligible_resource_ids = vec![sponsor];
        b.required_resource_ids = vec![sponsor, directory];
        let mut order = vec![
            LineItem {
                resource_id: sponsor,
                price_cents: 20_000,
            },
            LineItem {
                resource_id: classified,
                price_cents: 5_000,
            },
        ];
        // directory item missing -> bundle does not apply
        assert_eq!(quote(&order, std::slice::from_ref(&b)).discount_cents, 0);
        order.push(LineItem {
            resource_id: directory,
            price_cents: 9_000,
        });
        // applies, but only the sponsor item is discounted
        assert_eq!(quote(&order, &[b]).discount_cents, 10_000);
    }

    #[test]
    fn best_bundle_wins() {
        let small = bundle("percent_off", 2, 10, 0);
        let big = bundle("percent_off", 4, 25, 0);
        let q = quote(&items(Uuid::new_v4(), &[10_000; 4]), &[small, big.clone()]);
        assert_eq!(q.discount_cents, 10_000);
        assert_eq!(q.bundle_id, Some(big.id));
    }
}
