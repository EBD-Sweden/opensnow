-- Mart model: daily aggregated call summary per call type.
--
-- Provides key telecom KPIs:
--   - total calls
--   - successful / failed calls
--   - total and average duration
--   - unique callers and callees

with cdrs as (

    select * from {{ ref('stg_cdrs') }}

),

daily_summary as (

    select
        call_date,
        call_type,

        -- Volume
        count(*)                                            as total_calls,
        count(case when call_status = 'completed' then 1 end) as successful_calls,
        count(case when call_status = 'failed' then 1 end)    as failed_calls,
        count(case when call_status = 'dropped' then 1 end)   as dropped_calls,

        -- Duration
        sum(duration_seconds)                               as total_duration_seconds,
        avg(duration_seconds)                               as avg_duration_seconds,
        max(duration_seconds)                               as max_duration_seconds,

        -- Unique participants
        count(distinct caller_number)                       as unique_callers,
        count(distinct callee_number)                       as unique_callees,
        count(distinct cell_tower_id)                       as towers_used,

        -- Computed at build time
        current_date()                                      as built_at

    from cdrs
    group by call_date, call_type

)

select * from daily_summary
order by call_date desc, call_type
