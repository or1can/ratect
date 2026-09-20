export function formatVisitResponse(visitId, total, source) {
  return {
    message: "Hello from Ratect!",
    visit_id: visitId,
    total_visits: total,
    count_source: source,
  };
}
